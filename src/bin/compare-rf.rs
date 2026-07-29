use std::env;
use std::fs::File;
use std::io::{BufReader, Read, Write, BufWriter};
use std::path::{Path, PathBuf};

use rustfft::num_complex::Complex32;
use rustfft::FftPlanner;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

const WELCH_REFERENCE_BIN_HZ: f64 = 2441.41; // target bin width for matched comparison
const SPUR_THRESHOLD_DB: f64 = 10.0; // dB above median to flag as spur
const GUARD_TILT_THRESHOLD_DB: f64 = 6.0; // trigger validation sweep if guards differ by more
const APP_NAME: &str = "VHS RF Signal Analyser";
const APP_VERSION: &str = include_str!("../../VERSION");

// ─── Data structures ─────────────────────────────────────────────────────────

#[derive(Clone)]
struct Config {
    mode: RunMode,
    standard: String,
    window: String,
    orig_rate: u32,
    deci_rate: u32,
    orig_bits: Option<u32>,
    deci_bits: Option<u32>,
}

#[derive(Clone)]
enum RunMode {
    Compare {
        original: PathBuf,
        decimated: PathBuf,
        calibration: Option<PathBuf>,
        csv: Option<PathBuf>,
        psd_csv: Option<PathBuf>,
        json_out: Option<PathBuf>,
    },
    Calibrate {
        adc_file: PathBuf,
        chain_file: PathBuf,
        cal_output: PathBuf,
    },
}

#[allow(dead_code)]
#[derive(Clone)]
struct Band {
    name: &'static str,
    f_start_hz: f64,
    f_stop_hz: f64,
}

#[derive(Clone)]
struct RfPreset {
    name: &'static str,
    luma_sync_hz: f64,
    luma_white_hz: f64,
    chroma_carrier_hz: f64,
    luma_signal: Band,
    luma_sync_band: Band,
    luma_white_band: Band,
    luma_noise_low: Band,
    luma_noise_high: Band,
    chroma_signal: Band,
    chroma_noise_low: Band,
    chroma_noise_high: Band,
}

struct Metrics {
    bits: u32,
    sample_rate: u32,
    num_samples: u64,
    fft_bin_hz: f64,
    psd_dbfs_hz: Vec<f64>,  // PSD in dBFS/Hz
    mag_db: Vec<f64>,       // magnitude spectrum in dB (full-scale ref)
    peak_mag_db: Vec<f64>,  // averaged carrier-bin display magnitude in dB
    welch_segments: u64,
}

struct Spur {
    freq_hz: f64,
    amplitude_dbfs_hz: f64,
}

#[derive(Clone, Copy)]
struct FreqSpan {
    f_start_hz: f64,
    f_stop_hz: f64,
}

// ─── Accumulator (same perf-optimized version) ──────────────────────────────

struct Accum {
    sample_rate: u32,
    bits: u32,
    count: u64,
    fft_buf: Vec<f32>,
    segment_size: usize,
    nfft: usize,
    psd_accum: Vec<f64>,
    psd_segments: u64,
    window_coeffs: Vec<f32>,
    sum_w2: f64,
    fft: std::sync::Arc<dyn rustfft::Fft<f32>>,
    scratch: Vec<Complex32>,
    complex_buf: Vec<Complex32>,
}

impl Accum {
    fn new(sample_rate: u32, bits: u32, _window: &str) -> Self {
        // Scale FFT size to match reference bin width across sample rates
        let segment_size = {
            let raw = (sample_rate as f64 / WELCH_REFERENCE_BIN_HZ).round() as usize;
            raw.next_power_of_two().max(4096)
        };
        let nfft = segment_size;

        let denom = if segment_size > 1 { (segment_size - 1) as f32 } else { 1.0 };
        let mut window_coeffs = vec![0.0f32; segment_size];
        let mut sum_w2 = 0.0f64;
        // Blackman-Harris 4-term window: ~-92 dB sidelobe rejection
        let a0: f32 = 0.35875;
        let a1: f32 = 0.48829;
        let a2: f32 = 0.14128;
        let a3: f32 = 0.01168;
        for i in 0..segment_size {
            let x = i as f32 / denom;
            let pi2x = 2.0 * std::f32::consts::PI * x;
            let w = a0 - a1 * pi2x.cos() + a2 * (2.0 * pi2x).cos() - a3 * (3.0 * pi2x).cos();
            window_coeffs[i] = w;
            sum_w2 += (w as f64) * (w as f64);
        }

        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(nfft);
        let scratch = vec![Complex32::new(0.0, 0.0); fft.get_inplace_scratch_len()];
        let complex_buf = vec![Complex32::new(0.0, 0.0); nfft];

        Self {
            sample_rate, bits, count: 0,
            fft_buf: Vec::with_capacity(segment_size),
            segment_size, nfft,
            psd_accum: Vec::new(), psd_segments: 0,
            window_coeffs, sum_w2, fft, scratch, complex_buf,
        }
    }

    fn push_batch(&mut self, samples: &[f32]) {
        let mut offset = 0;
        while offset < samples.len() {
            let space = self.segment_size - self.fft_buf.len();
            let end = (offset + space).min(samples.len());
            let chunk = &samples[offset..end];
            self.count += chunk.len() as u64;
            self.fft_buf.extend_from_slice(chunk);
            offset = end;
            if self.fft_buf.len() >= self.segment_size {
                self.process_segment();
            }
        }
    }

    fn process_segment(&mut self) {
        let n = self.fft_buf.len();
        let nfft = self.nfft;
        let half = nfft / 2;
        for i in 0..nfft {
            if i < n {
                self.complex_buf[i] = Complex32::new(self.fft_buf[i] * self.window_coeffs[i.min(self.window_coeffs.len() - 1)], 0.0);
            } else {
                self.complex_buf[i] = Complex32::new(0.0, 0.0);
            }
        }
        self.fft.process_with_scratch(&mut self.complex_buf, &mut self.scratch);
        let norm = (self.sample_rate as f64 * self.sum_w2).max(1e-12);
        if self.psd_accum.is_empty() {
            self.psd_accum = vec![0.0f64; half];
        }
        for i in 0..half {
            let mut psd = self.complex_buf[i].norm_sqr() as f64 / norm;
            if i > 0 && i < (half - 1) { psd *= 2.0; }
            self.psd_accum[i] += psd.max(1e-24);
        }
        self.psd_segments += 1;
        self.fft_buf.clear();
    }

    fn finalize(mut self) -> Metrics {
        if !self.fft_buf.is_empty() && (self.psd_segments == 0 || self.fft_buf.len() >= self.segment_size / 2) {
            self.process_segment();
        }
        let segments = self.psd_segments.max(1) as f64;
        for p in self.psd_accum.iter_mut() { *p /= segments; }
        let fft_bin_hz = self.sample_rate as f64 / self.nfft as f64;

        // Normalize to dBFS/Hz: divide by full_scale^2
        // Full scale amplitude = 2^(bits-1) for signed data
        let full_scale = (1u64 << (self.bits - 1)) as f64;
        let fs_sq = full_scale * full_scale;
        let psd_dbfs_hz: Vec<f64> = self.psd_accum.iter()
            .map(|&p| 10.0 * (p / fs_sq).max(1e-30).log10())
            .collect();

        // Magnitude dB: convert PSD back to per-bin magnitude for cross-checking
        // mag_db = psd_dbfs_hz + 10*log10(bin_hz * noise_bw_correction)
        // For Blackman-Harris, noise bandwidth factor ~ 2.0044
        let noise_bw = fft_bin_hz * 2.0044;
        let bw_correction = 10.0 * noise_bw.log10();
        let mag_db: Vec<f64> = psd_dbfs_hz.iter()
            .map(|&p| p + bw_correction)
            .collect();
        let coherent_gain = self.window_coeffs.iter().map(|&w| w as f64).sum::<f64>() / self.segment_size as f64;
        let carrier_display_correction_db = -20.0 * coherent_gain.max(1e-12).log10() - 10.0 * 2.0f64.log10();
        let peak_mag_db: Vec<f64> = mag_db.iter()
            .map(|&p| p + carrier_display_correction_db)
            .collect();

        Metrics {
            bits: self.bits, sample_rate: self.sample_rate, num_samples: self.count,
            fft_bin_hz,
            psd_dbfs_hz, mag_db, peak_mag_db,
            welch_segments: self.psd_segments,
        }
    }
}

// ─── Analysis helpers ────────────────────────────────────────────────────────

fn peak_in_span_mag_db(m: &Metrics, span: FreqSpan) -> (f64, f64) {
    let start = ((span.f_start_hz / m.fft_bin_hz).floor() as usize).min(m.peak_mag_db.len().saturating_sub(1));
    let stop = ((span.f_stop_hz / m.fft_bin_hz).ceil() as usize).min(m.peak_mag_db.len().saturating_sub(1));
    let mut max_val = f64::NEG_INFINITY;
    let mut max_idx = start;
    for i in start..=stop {
        if m.peak_mag_db[i] > max_val {
            max_val = m.peak_mag_db[i];
            max_idx = i;
        }
    }
    (max_val, max_idx as f64 * m.fft_bin_hz)
}

fn peak_in_span_mag_db_near_nominal(m: &Metrics, span: FreqSpan, nominal_hz: f64) -> (f64, f64) {
    let start = ((span.f_start_hz / m.fft_bin_hz).floor() as usize).min(m.peak_mag_db.len().saturating_sub(1));
    let stop = ((span.f_stop_hz / m.fft_bin_hz).ceil() as usize).min(m.peak_mag_db.len().saturating_sub(1));
    let mut max_val = f64::NEG_INFINITY;
    for i in start..=stop {
        if m.peak_mag_db[i] > max_val {
            max_val = m.peak_mag_db[i];
        }
    }

    let tie_tolerance_db = 0.25;
    let mut best_idx = start;
    let mut best_dist = f64::INFINITY;
    for i in start..=stop {
        if m.peak_mag_db[i] >= max_val - tie_tolerance_db {
            let freq_hz = i as f64 * m.fft_bin_hz;
            let dist = (freq_hz - nominal_hz).abs();
            if dist < best_dist {
                best_dist = dist;
                best_idx = i;
            }
        }
    }

    (m.peak_mag_db[best_idx], best_idx as f64 * m.fft_bin_hz)
}

fn guard_floor_mag_db(m: &Metrics, band: &Band) -> f64 {
    let start = ((band.f_start_hz / m.fft_bin_hz).floor() as usize).min(m.mag_db.len().saturating_sub(1));
    let stop = ((band.f_stop_hz / m.fft_bin_hz).ceil() as usize).min(m.mag_db.len().saturating_sub(1));
    let mut min_val = f64::MAX;
    for i in start..=stop {
        if m.mag_db[i] < min_val {
            min_val = m.mag_db[i];
        }
    }
    if min_val == f64::MAX { -200.0 } else { min_val }
}

// Returns (magnitude_dB, frequency_Hz) of the minimum noise floor in the guard band
fn guard_floor_mag_db_with_freq(m: &Metrics, band: &Band) -> (f64, f64) {
    let start = ((band.f_start_hz / m.fft_bin_hz).floor() as usize).min(m.mag_db.len().saturating_sub(1));
    let stop = ((band.f_stop_hz / m.fft_bin_hz).ceil() as usize).min(m.mag_db.len().saturating_sub(1));
    let mut min_val = f64::MAX;
    let mut min_idx = start;
    for i in start..=stop {
        if m.mag_db[i] < min_val {
            min_val = m.mag_db[i];
            min_idx = i;
        }
    }
    let freq = min_idx as f64 * m.fft_bin_hz;
    if min_val == f64::MAX { (-200.0, freq) } else { (min_val, freq) }
}

/// Noise floor from guard bands: find the lowest magnitude bin across both
/// guard bands (and swept intermediate signal-free regions if tilt is large).
/// Returns the actual minimum value and the frequency where it was found.
fn validated_floor_mag_db(m: &Metrics, low_guard: &Band, high_guard: &Band,
                          _carrier_hz: f64, signal_bands: &[FreqSpan]) -> (f64, String) {
    let (floor_low, freq_low) = guard_floor_mag_db_with_freq(m, low_guard);
    let (floor_high, freq_high) = guard_floor_mag_db_with_freq(m, high_guard);
    let tilt = (floor_high - floor_low).abs();

    // Start with the lowest of the two guard-band minima
    let (mut best_floor, mut best_freq) = if floor_low <= floor_high {
        (floor_low, freq_low)
    } else {
        (floor_high, freq_high)
    };

    // If guards tilt significantly, also sweep signal-free regions between them
    // to check for an even lower point
    if tilt > GUARD_TILT_THRESHOLD_DB {
        let sweep_start = low_guard.f_stop_hz;
        let sweep_stop = high_guard.f_start_hz;
        let step_hz = 500_000.0;

        let mut f = sweep_start;
        while f < sweep_stop {
            let f_end = (f + step_hz).min(sweep_stop);
            let span = FreqSpan { f_start_hz: f, f_stop_hz: f_end };

            let overlaps_signal = signal_bands.iter().any(|sb| {
                span.f_start_hz < sb.f_stop_hz && span.f_stop_hz > sb.f_start_hz
            });

            if !overlaps_signal {
                let (floor_here, freq_here) = guard_floor_mag_db_with_freq(m, &Band { name: "", f_start_hz: f, f_stop_hz: f_end });
                if floor_here < best_floor {
                    best_floor = floor_here;
                    best_freq = freq_here;
                }
            }
            f = f_end;
        }
    }

    (best_floor, format!("at {:.2} MHz", best_freq / 1e6))
}

fn noise_floor_dbfs(m: &Metrics, low: &Band, high: &Band) -> f64 {
    // Median PSD in guard bands (dBFS/Hz)
    let mut values = Vec::new();
    let add_band = |vals: &mut Vec<f64>, band: &Band| {
        let start = ((band.f_start_hz / m.fft_bin_hz).floor() as usize).min(m.psd_dbfs_hz.len().saturating_sub(1));
        let stop = ((band.f_stop_hz / m.fft_bin_hz).ceil() as usize).min(m.psd_dbfs_hz.len().saturating_sub(1));
        for i in start..=stop {
            vals.push(m.psd_dbfs_hz[i]);
        }
    };
    add_band(&mut values, low);
    add_band(&mut values, high);
    if values.is_empty() { return -200.0; }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    values[values.len() / 2]
}

fn enob(snr_dbfs: f64) -> f64 {
    (snr_dbfs - 1.76) / 6.02
}

fn detect_spurs(m: &Metrics, exclude_bands: &[&Band]) -> Vec<Spur> {
    // Find peaks above median + SPUR_THRESHOLD_DB, excluding known signal bands
    let mut all: Vec<f64> = m.psd_dbfs_hz.clone();
    all.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = if all.is_empty() { -200.0 } else { all[all.len() / 2] };
    let threshold = median + SPUR_THRESHOLD_DB;

    let is_excluded = |freq_hz: f64| -> bool {
        for band in exclude_bands {
            if freq_hz >= band.f_start_hz && freq_hz <= band.f_stop_hz {
                return true;
            }
        }
        false
    };

    let mut spurs = Vec::new();
    let len = m.psd_dbfs_hz.len();
    for i in 2..len.saturating_sub(2) {
        let v = m.psd_dbfs_hz[i];
        if v > threshold
            && v >= m.psd_dbfs_hz[i - 1] && v >= m.psd_dbfs_hz[i + 1]
            && v >= m.psd_dbfs_hz[i - 2] && v >= m.psd_dbfs_hz[i + 2]
        {
            let freq = i as f64 * m.fft_bin_hz;
            if !is_excluded(freq) {
                spurs.push(Spur { freq_hz: freq, amplitude_dbfs_hz: v });
            }
        }
    }
    // Keep top 10 by amplitude
    spurs.sort_by(|a, b| b.amplitude_dbfs_hz.partial_cmp(&a.amplitude_dbfs_hz).unwrap_or(std::cmp::Ordering::Equal));
    spurs.truncate(10);
    spurs
}

fn write_psd_csv(path: &Path, label: &str, m: &Metrics) -> Result<(), Box<dyn std::error::Error>> {
    let mut f = BufWriter::new(File::create(path)?);
    writeln!(f, "freq_hz,{}_dbfs_hz", label)?;
    for (i, &v) in m.psd_dbfs_hz.iter().enumerate() {
        writeln!(f, "{:.2},{:.6}", i as f64 * m.fft_bin_hz, v)?;
    }
    Ok(())
}

// ─── Baseline - Profile ADC+Signal Chain noise level ─────────────────────────────────────────────────────────────

fn run_calibrate(cfg: &Config, adc_file: &Path, chain_file: &Path, cal_output: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let preset = rf_preset(&cfg.standard)?;

    eprintln!("=== Profile ADC Mode ===");
    eprintln!("Standard: {}", preset.name);
    eprintln!("Loading ADC-only capture: {}", adc_file.display());
    let adc = analyze_file(adc_file, cfg.orig_rate, cfg.orig_bits, &cfg.window)?;

    eprintln!("Loading chain (blank tape) capture: {}", chain_file.display());
    let chain = analyze_file(chain_file, cfg.orig_rate, cfg.orig_bits, &cfg.window)?;

    let exclude_bands: Vec<&Band> = vec![
        &preset.luma_signal, &preset.chroma_signal,
    ];
    let adc_spurs = detect_spurs(&adc, &exclude_bands);
    let chain_spurs = detect_spurs(&chain, &exclude_bands);

    // Merge spurs (within 2 bins = same spur)
    let mut all_spurs = Vec::new();
    for s in adc_spurs.iter().chain(chain_spurs.iter()) {
        let dominated = all_spurs.iter().any(|existing: &Spur| (existing.freq_hz - s.freq_hz).abs() < 2.0 * adc.fft_bin_hz);
        if !dominated {
            all_spurs.push(Spur { freq_hz: s.freq_hz, amplitude_dbfs_hz: s.amplitude_dbfs_hz });
        }
    }

    let adc_nsd_luma = noise_floor_dbfs(&adc, &preset.luma_noise_low, &preset.luma_noise_high);
    let chain_nsd_luma = noise_floor_dbfs(&chain, &preset.luma_noise_low, &preset.luma_noise_high);
    let adc_nsd_chroma = noise_floor_dbfs(&adc, &preset.chroma_noise_low, &preset.chroma_noise_high);
    let chain_nsd_chroma = noise_floor_dbfs(&chain, &preset.chroma_noise_low, &preset.chroma_noise_high);

    // Build JSON Profile/Baseline file
    let mut f = BufWriter::new(File::create(cal_output)?);
    writeln!(f, "{{")?;
    writeln!(f, "  \"version\": 1,")?;
    writeln!(f, "  \"sample_rate\": {}," , cfg.orig_rate)?;
    writeln!(f, "  \"bits\": {}," , cfg.orig_bits.unwrap_or(adc.bits))?;
    writeln!(f, "  \"standard\": \"{}\",", cfg.standard)?;
    writeln!(f, "  \"fft_bin_hz\": {:.4},", adc.fft_bin_hz)?;
    writeln!(f, "  \"adc_nsd_luma_dbfs_hz\": {:.6},", adc_nsd_luma)?;
    writeln!(f, "  \"chain_nsd_luma_dbfs_hz\": {:.6},", chain_nsd_luma)?;
    writeln!(f, "  \"adc_nsd_chroma_dbfs_hz\": {:.6},", adc_nsd_chroma)?;
    writeln!(f, "  \"chain_nsd_chroma_dbfs_hz\": {:.6},", chain_nsd_chroma)?;

    // Write full PSD arrays
    write!(f, "  \"adc_psd_dbfs_hz\": [")?;
    for (i, v) in adc.psd_dbfs_hz.iter().enumerate() {
        if i > 0 { write!(f, ",")?; }
        write!(f, "{:.4}", v)?;
    }
    writeln!(f, "],")?;

    write!(f, "  \"chain_psd_dbfs_hz\": [")?;
    for (i, v) in chain.psd_dbfs_hz.iter().enumerate() {
        if i > 0 { write!(f, ",")?; }
        write!(f, "{:.4}", v)?;
    }
    writeln!(f, "],")?;

    write!(f, "  \"spurs\": [")?;
    for (i, s) in all_spurs.iter().enumerate() {
        if i > 0 { write!(f, ",")?; }
        write!(f, "{{\"freq_hz\":{:.2},\"amplitude_dbfs_hz\":{:.4}}}", s.freq_hz, s.amplitude_dbfs_hz)?;
    }
    writeln!(f, "]")?;
    writeln!(f, "}}")?;

    eprintln!("Calibration saved: {}", cal_output.display());
    eprintln!("ADC NSD luma:   {:.2} dBFS/Hz", adc_nsd_luma);
    eprintln!("Chain NSD luma: {:.2} dBFS/Hz", chain_nsd_luma);
    eprintln!("VCR+amp luma:   {:.2} dB above ADC", chain_nsd_luma - adc_nsd_luma);
    eprintln!("ADC NSD chroma: {:.2} dBFS/Hz", adc_nsd_chroma);
    eprintln!("Chain NSD chroma:{:.2} dBFS/Hz", chain_nsd_chroma);
    eprintln!("Signal spurs found: {}", all_spurs.len());
    for s in &all_spurs {
        eprintln!("  {:.3} kHz at {:.2} dBFS/Hz", s.freq_hz / 1e3, s.amplitude_dbfs_hz);
    }

    // Print confirmation to stdout for GUI
    println!("PROFILING_COMPLETE");
    println!("cal_file={}", cal_output.display());
    println!("adc_nsd_luma={:.6}", adc_nsd_luma);
    println!("chain_nsd_luma={:.6}", chain_nsd_luma);
    println!("spurs_found={}", all_spurs.len());

    Ok(())
}

// ─── Main compare ────────────────────────────────────────────────────────────

fn run_compare(cfg: &Config, original: &Path, decimated: &Path,
               _calibration: &Option<PathBuf>, csv: &Option<PathBuf>,
               psd_csv: &Option<PathBuf>, json_out: &Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let preset = rf_preset(&cfg.standard)?;
    let dec_factor = cfg.orig_rate as f64 / cfg.deci_rate as f64;

    eprintln!("======================================================================");
    eprintln!("{} v{}", APP_NAME, APP_VERSION.trim());
    eprintln!("======================================================================");

    eprintln!("\nLoading original: {}", original.display());
    eprintln!("Loading Comparison: {}", decimated.display());

    // Analyze both files in parallel for ~2x speedup
    let orig_path = original.to_path_buf();
    let orig_rate = cfg.orig_rate;
    let deci_rate = cfg.deci_rate;
    let orig_bits = cfg.orig_bits;
    let deci_bits = cfg.deci_bits;
    let window = cfg.window.clone();

    let orig_handle = std::thread::spawn(move || -> Result<Metrics, String> {
        analyze_file(&orig_path, orig_rate, orig_bits, &window).map_err(|e| e.to_string())
    });
    let deci_result = analyze_file(decimated, deci_rate, deci_bits, &cfg.window);
    let orig = orig_handle.join().map_err(|_| "original file analysis thread panicked")?
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    let deci = deci_result?;
    let orig_duration_s = orig.num_samples as f64 / orig.sample_rate as f64;
    let deci_duration_s = deci.num_samples as f64 / deci.sample_rate as f64;

    let luma_full_span = FreqSpan { f_start_hz: preset.luma_signal.f_start_hz, f_stop_hz: preset.luma_signal.f_stop_hz };
    let sync_search_span = FreqSpan { f_start_hz: preset.luma_sync_band.f_start_hz, f_stop_hz: preset.luma_sync_band.f_stop_hz };
    let white_search_span = FreqSpan { f_start_hz: preset.luma_white_band.f_start_hz, f_stop_hz: preset.luma_white_band.f_stop_hz };
    let chroma_search_span = FreqSpan { f_start_hz: preset.chroma_signal.f_start_hz, f_stop_hz: preset.chroma_signal.f_stop_hz };

    // Signal bands for sweep exclusion
    let signal_bands = vec![luma_full_span, chroma_search_span];

    // Magnitude-dB peaks (cross-checkable misrc_gui)
    let (orig_luma_peak_mag, orig_luma_peak_freq) = peak_in_span_mag_db(&orig, luma_full_span);
    let (deci_luma_peak_mag, deci_luma_peak_freq) = peak_in_span_mag_db(&deci, luma_full_span);
    let (orig_sync_peak_mag, orig_sync_peak_freq) = peak_in_span_mag_db(&orig, sync_search_span);
    let (deci_sync_peak_mag, deci_sync_peak_freq) = peak_in_span_mag_db(&deci, sync_search_span);
    let (orig_white_peak_mag, orig_white_peak_freq) = peak_in_span_mag_db(&orig, white_search_span);
    let (deci_white_peak_mag, deci_white_peak_freq) = peak_in_span_mag_db(&deci, white_search_span);
    let (orig_chroma_peak_mag, orig_chroma_peak_freq) = peak_in_span_mag_db_near_nominal(&orig, chroma_search_span, preset.chroma_carrier_hz);
    let (deci_chroma_peak_mag, deci_chroma_peak_freq) = peak_in_span_mag_db_near_nominal(&deci, chroma_search_span, preset.chroma_carrier_hz);

    // Guard-band noise floors in magnitude dB. Guard-Band = Likely noise floor frequency
    let orig_floor_low_mag = guard_floor_mag_db(&orig, &preset.luma_noise_low);
    let deci_floor_low_mag = guard_floor_mag_db(&deci, &preset.luma_noise_low);
    let orig_floor_high_mag = guard_floor_mag_db(&orig, &preset.luma_noise_high);
    let deci_floor_high_mag = guard_floor_mag_db(&deci, &preset.luma_noise_high);
    // Chroma floor: use HIGH guard only (950 kHz-1.3 MHz).
    // Low guard (225-400 kHz) is too far below the ~629 kHz carrier and measures
    // spectral rolloff near DC, not true noise - giving artificially low floor and
    // inflated SNR (~52 dB vs expected VHS ~40-45 dB).
    // let (orig_chroma_floor_mag, orig_chroma_floor_method) = validated_floor_mag_db(
    //     &orig, &preset.chroma_noise_low, &preset.chroma_noise_high,
    //     orig_chroma_peak_freq, &signal_bands);
    // let (deci_chroma_floor_mag, deci_chroma_floor_method) = validated_floor_mag_db(
    //     &deci, &preset.chroma_noise_low, &preset.chroma_noise_high,
    //     deci_chroma_peak_freq, &signal_bands);
    let (orig_chroma_floor_mag, orig_chroma_floor_method) = validated_floor_mag_db(
        &orig, &preset.chroma_noise_high, &preset.chroma_noise_high,
        orig_chroma_peak_freq, &signal_bands);
    let (deci_chroma_floor_mag, deci_chroma_floor_method) = validated_floor_mag_db(
        &deci, &preset.chroma_noise_high, &preset.chroma_noise_high,
        deci_chroma_peak_freq, &signal_bands);

    // Determine noise floors by sweeping if tilt exceeds threshold
    let (orig_sync_floor_mag, orig_sync_floor_method) = validated_floor_mag_db(
        &orig, &preset.luma_noise_low, &preset.luma_noise_high,
        orig_sync_peak_freq, &signal_bands);
    let (deci_sync_floor_mag, deci_sync_floor_method) = validated_floor_mag_db(
        &deci, &preset.luma_noise_low, &preset.luma_noise_high,
        deci_sync_peak_freq, &signal_bands);
    let (orig_white_floor_mag, orig_white_floor_method) = validated_floor_mag_db(
        &orig, &preset.luma_noise_low, &preset.luma_noise_high,
        orig_white_peak_freq, &signal_bands);
    let (deci_white_floor_mag, deci_white_floor_method) = validated_floor_mag_db(
        &deci, &preset.luma_noise_low, &preset.luma_noise_high,
        deci_white_peak_freq, &signal_bands);
    let (orig_luma_floor_mag, _orig_luma_floor_method) = validated_floor_mag_db(
        &orig, &preset.luma_noise_low, &preset.luma_noise_high,
        (preset.luma_sync_hz + preset.luma_white_hz) * 0.5, &signal_bands);
    let (deci_luma_floor_mag, _deci_luma_floor_method) = validated_floor_mag_db(
        &deci, &preset.luma_noise_low, &preset.luma_noise_high,
        (preset.luma_sync_hz + preset.luma_white_hz) * 0.5, &signal_bands);

    // SNR = peak magnitude dB minus floor magnitude dB
    let orig_luma_snr = orig_luma_peak_mag - orig_luma_floor_mag;
    let deci_luma_snr = deci_luma_peak_mag - deci_luma_floor_mag;
    let orig_sync_snr = orig_sync_peak_mag - orig_sync_floor_mag;
    let deci_sync_snr = deci_sync_peak_mag - deci_sync_floor_mag;
    let orig_white_snr = orig_white_peak_mag - orig_white_floor_mag;
    let deci_white_snr = deci_white_peak_mag - deci_white_floor_mag;
    let orig_chroma_snr = orig_chroma_peak_mag - orig_chroma_floor_mag;
    let deci_chroma_snr = deci_chroma_peak_mag - deci_chroma_floor_mag;

    let orig_enob = enob(orig_luma_snr);
    let deci_enob = enob(deci_luma_snr);

    // ─── Report ──────────────────────────────────────────────────────────
    println!("{} v{}", APP_NAME, APP_VERSION.trim());
	println!("-------------------------------");
    println!("Standard: {}", preset.name);
    println!("Decimation factor: {:.0}", dec_factor);
    println!("Window: Blackman-Harris 4-term");
	println!("By ZL3RXT - For VHS Decode 2026");
	println!("-------------------------------");
    println!();

    println!("Input Summary");
	println!("-------------");
    println!("- Original:  {} Hz, {}-bit, {} samples, {} Welch segments, FFT {}",
        orig.sample_rate, orig.bits, format_with_commas(orig.num_samples), orig.welch_segments,
        (orig.sample_rate as f64 / orig.fft_bin_hz).round() as usize);
    println!("  Duration:  {:.3} s ({})", orig_duration_s, format_duration_hms(orig_duration_s));
    println!("- Comparison: {} Hz, {}-bit, {} samples, {} Welch segments, FFT {}",
        deci.sample_rate, deci.bits, format_with_commas(deci.num_samples), deci.welch_segments,
        (deci.sample_rate as f64 / deci.fft_bin_hz).round() as usize);
    println!("  Duration:  {:.3} s ({})", deci_duration_s, format_duration_hms(deci_duration_s));
    println!("- FFT bin width: {:.2} Hz (orig) / {:.2} Hz (deci)", orig.fft_bin_hz, deci.fft_bin_hz);
    println!();

    println!("Signal Peaks [dB magnitude, full-scale]");
	println!("------------------------------------------------------------------");
    println!("- Sync peak original:        {:.2} dB at {:.6} MHz", orig_sync_peak_mag, orig_sync_peak_freq / 1e6);
    println!("- Sync peak comparison:      {:.2} dB at {:.6} MHz", deci_sync_peak_mag, deci_sync_peak_freq / 1e6);
	println!();
    println!("- White peak original:       {:.2} dB at {:.6} MHz", orig_white_peak_mag, orig_white_peak_freq / 1e6);
    println!("- White peak comparison:     {:.2} dB at {:.6} MHz", deci_white_peak_mag, deci_white_peak_freq / 1e6);
	println!();
	println!("- Chroma peak original:      {:.2} dB at {:.6} MHz", orig_chroma_peak_mag, orig_chroma_peak_freq / 1e6);
	println!("- Chroma peak Comparison:    {:.2} dB at {:.6} MHz", deci_chroma_peak_mag, deci_chroma_peak_freq / 1e6);
    println!();

    println!("Noise Floors [dB magnitude] (Lowest measured per guard band area)");
	println!("------------------------------------------------------------------");
    println!("- Low guard ({:.1}-{:.1} MHz)  original: {:.2} dB  comparison: {:.2} dB",
        preset.luma_noise_low.f_start_hz / 1e6, preset.luma_noise_low.f_stop_hz / 1e6,
        orig_floor_low_mag, deci_floor_low_mag);
    println!("- High guard ({:.1}-{:.1} MHz) original: {:.2} dB  comparison: {:.2} dB",
        preset.luma_noise_high.f_start_hz / 1e6, preset.luma_noise_high.f_stop_hz / 1e6,
        orig_floor_high_mag, deci_floor_high_mag);
    println!("- Guard tilt original: {:+.2} dB  comparison: {:+.2} dB",
        orig_floor_high_mag - orig_floor_low_mag,
        deci_floor_high_mag - deci_floor_low_mag);
	println!();   
    println!("+ Sync floor original:       {:.2} dB  ({})", orig_sync_floor_mag, orig_sync_floor_method);
    println!("- Sync floor comparison:     {:.2} dB  ({})", deci_sync_floor_mag, deci_sync_floor_method);
	println!();
    println!("+ White floor original:      {:.2} dB  ({})", orig_white_floor_mag, orig_white_floor_method);
    println!("- White floor comparison:    {:.2} dB  ({})", deci_white_floor_mag, deci_white_floor_method);
	println!();
    println!("+ Chroma floor original:     {:.2} dB  ({})", orig_chroma_floor_mag, orig_chroma_floor_method);
    println!("- Chroma floor comparison:   {:.2} dB  ({})", deci_chroma_floor_mag, deci_chroma_floor_method);
    println!();

    println!("SNR [dB] (peak - noise floor)");
	println!("------------------------------------------------------------------");
    println!("+ Sync SNR original:         {:.2} dB", orig_sync_snr);
    println!("- Sync SNR comparison:       {:.2} dB", deci_sync_snr);
    println!("- Sync SNR delta:            {:+.2} dB", deci_sync_snr - orig_sync_snr);
    println!();	
    println!("+ White SNR original:        {:.2} dB", orig_white_snr);
    println!("- White SNR comparison:      {:.2} dB", deci_white_snr);
    println!("- White SNR delta:           {:+.2} dB", deci_white_snr - orig_white_snr);
	println!();
    println!("+ Chroma SNR original:       {:.2} dB", orig_chroma_snr);
    println!("- Chroma SNR comparison:     {:.2} dB", deci_chroma_snr);
    println!("- Chroma SNR delta:          {:+.2} dB", deci_chroma_snr - orig_chroma_snr);
    println!();

    println!("VHS Spec Reference Metrics");
	println!("------------------------------------------------------------------");
    println!("+ Sync tip reference:        {:.6} MHz", preset.luma_sync_hz / 1e6);
    println!("- Sync carrier original:     {:.6} MHz ({:+.2} kHz from ref)", orig_sync_peak_freq / 1e6, (orig_sync_peak_freq - preset.luma_sync_hz) / 1e3);
    println!("- Sync carrier comparison:   {:.6} MHz ({:+.2} kHz from ref)", deci_sync_peak_freq / 1e6, (deci_sync_peak_freq - preset.luma_sync_hz) / 1e3);
    println!();
	println!("+ White peak reference:      {:.6} MHz", preset.luma_white_hz / 1e6);
    println!("- White carrier original:    {:.6} MHz ({:+.2} kHz from ref)", orig_white_peak_freq / 1e6, (orig_white_peak_freq - preset.luma_white_hz) / 1e3);
    println!("- White carrier comparison:  {:.6} MHz ({:+.2} kHz from ref)", deci_white_peak_freq / 1e6, (deci_white_peak_freq - preset.luma_white_hz) / 1e3);
    println!();
	println!("+ Luma deviation reference:  {:.2} kHz", (preset.luma_white_hz - preset.luma_sync_hz) / 1e3);
    println!("- Luma deviation original:   {:.2} kHz ({:+.2} kHz from ref)",
        (orig_white_peak_freq - orig_sync_peak_freq) / 1e3,
        ((orig_white_peak_freq - orig_sync_peak_freq) - (preset.luma_white_hz - preset.luma_sync_hz)) / 1e3);
    println!("- Luma deviation comparison: {:.2} kHz ({:+.2} kHz from ref)",
        (deci_white_peak_freq - deci_sync_peak_freq) / 1e3,
        ((deci_white_peak_freq - deci_sync_peak_freq) - (preset.luma_white_hz - preset.luma_sync_hz)) / 1e3);
    println!();
	println!("+ Chroma carrier reference:  {:.6} MHz", preset.chroma_carrier_hz / 1e6);
    println!("- Chroma carrier original:   {:.6} MHz ({:+.2} kHz from ref)", orig_chroma_peak_freq / 1e6, (orig_chroma_peak_freq - preset.chroma_carrier_hz) / 1e3);
    println!("- Chroma carrier comparison: {:.6} MHz ({:+.2} kHz from ref)", deci_chroma_peak_freq / 1e6, (deci_chroma_peak_freq - preset.chroma_carrier_hz) / 1e3);
    println!();

    println!("Derived Summary");
	println!("------------------------------------------------------------------");
    println!("+ Equivalent SNR bits orig:  {:.1} bits", orig_enob);
    println!("- Equivalent SNR bits comp:  {:.1} bits", deci_enob);
    println!("  *SNR converted to bits. VHS typically 5-7bits, Aim for: 7.6 bits");
    println!("  **Aim for: 7.6 bits");
	println!();
    println!("+ ADC quantization ceiling:  {:.1} dB / {:.1} dB ({}-bit / {}-bit)",
        6.02 * orig.bits as f64 + 1.76, 6.02 * deci.bits as f64 + 1.76, orig.bits, deci.bits);
    println!("  *Maximum possible SNR for the given bit depth.");
	println!("   VHS SNR is always below max - refer specs");
	println!("   Aim for: 74dB, 12-Bit @40MSPS");
    println!();
    println!();

      // Write PSD CSV for plotting
    if let Some(psd_prefix) = psd_csv {
        let orig_path = psd_prefix.with_extension("orig.psd.csv");
        let deci_path = psd_prefix.with_extension("deci.psd.csv");
        write_psd_csv(&orig_path, "original", &orig)?;
        write_psd_csv(&deci_path, "decimated", &deci)?;
        eprintln!("PSD CSV written: {}, {}", orig_path.display(), deci_path.display());
    }

    // Write metrics CSV
    if let Some(csv_path) = csv {
        let mut f = File::create(csv_path)?;
        writeln!(f, "metric,original,decimated,delta,unit")?;
        writeln!(f, "luma_peak,{:.6},{:.6},{:.6},dB_mag", orig_luma_peak_mag, deci_luma_peak_mag, deci_luma_peak_mag - orig_luma_peak_mag)?;
        writeln!(f, "sync_peak,{:.6},{:.6},{:.6},dB_mag", orig_sync_peak_mag, deci_sync_peak_mag, deci_sync_peak_mag - orig_sync_peak_mag)?;
        writeln!(f, "white_peak,{:.6},{:.6},{:.6},dB_mag", orig_white_peak_mag, deci_white_peak_mag, deci_white_peak_mag - orig_white_peak_mag)?;
        writeln!(f, "chroma_peak,{:.6},{:.6},{:.6},dB_mag", orig_chroma_peak_mag, deci_chroma_peak_mag, deci_chroma_peak_mag - orig_chroma_peak_mag)?;
        writeln!(f, "luma_snr,{:.6},{:.6},{:.6},dB", orig_luma_snr, deci_luma_snr, deci_luma_snr - orig_luma_snr)?;
        writeln!(f, "sync_snr,{:.6},{:.6},{:.6},dB", orig_sync_snr, deci_sync_snr, deci_sync_snr - orig_sync_snr)?;
        writeln!(f, "white_snr,{:.6},{:.6},{:.6},dB", orig_white_snr, deci_white_snr, deci_white_snr - orig_white_snr)?;
        writeln!(f, "chroma_snr,{:.6},{:.6},{:.6},dB", orig_chroma_snr, deci_chroma_snr, deci_chroma_snr - orig_chroma_snr)?;
        writeln!(f, "luma_floor,{:.6},{:.6},{:.6},dB_mag", orig_luma_floor_mag, deci_luma_floor_mag, deci_luma_floor_mag - orig_luma_floor_mag)?;
        writeln!(f, "sync_floor,{:.6},{:.6},{:.6},dB_mag", orig_sync_floor_mag, deci_sync_floor_mag, deci_sync_floor_mag - orig_sync_floor_mag)?;
        writeln!(f, "white_floor,{:.6},{:.6},{:.6},dB_mag", orig_white_floor_mag, deci_white_floor_mag, deci_white_floor_mag - orig_white_floor_mag)?;
        writeln!(f, "chroma_floor,{:.6},{:.6},{:.6},dB_mag", orig_chroma_floor_mag, deci_chroma_floor_mag, deci_chroma_floor_mag - orig_chroma_floor_mag)?;
        writeln!(f, "enob,{:.2},{:.2},{:.2},bits", orig_enob, deci_enob, deci_enob - orig_enob)?;
    }

    // Write JSON for GUI parsing
    if let Some(json_path) = json_out {
        let mut f = BufWriter::new(File::create(json_path)?);
        writeln!(f, "{{")?;
        writeln!(f, "  \"version\": \"{}\",", APP_VERSION.trim())?;
        writeln!(f, "  \"standard\": \"{}\",", preset.name)?;
        writeln!(f, "  \"dec_factor\": {:.0},", dec_factor)?;
        writeln!(f, "  \"orig\": {{ \"rate\": {}, \"bits\": {}, \"samples\": {}, \"welch_segments\": {} }},",
            orig.sample_rate, orig.bits, orig.num_samples, orig.welch_segments)?;
        writeln!(f, "  \"deci\": {{ \"rate\": {}, \"bits\": {}, \"samples\": {}, \"welch_segments\": {} }},",
            deci.sample_rate, deci.bits, deci.num_samples, deci.welch_segments)?;
        writeln!(f, "  \"luma_snr\": {{ \"orig\": {:.4}, \"deci\": {:.4}, \"delta\": {:.4} }},",
            orig_luma_snr, deci_luma_snr, deci_luma_snr - orig_luma_snr)?;
        writeln!(f, "  \"sync_snr\": {{ \"orig\": {:.4}, \"deci\": {:.4}, \"delta\": {:.4} }},",
            orig_sync_snr, deci_sync_snr, deci_sync_snr - orig_sync_snr)?;
        writeln!(f, "  \"white_snr\": {{ \"orig\": {:.4}, \"deci\": {:.4}, \"delta\": {:.4} }},",
            orig_white_snr, deci_white_snr, deci_white_snr - orig_white_snr)?;
        writeln!(f, "  \"chroma_snr\": {{ \"orig\": {:.4}, \"deci\": {:.4}, \"delta\": {:.4} }},",
            orig_chroma_snr, deci_chroma_snr, deci_chroma_snr - orig_chroma_snr)?;
        writeln!(f, "  \"noise_floor_luma\": {{ \"orig\": {:.4}, \"deci\": {:.4} }},", orig_luma_floor_mag, deci_luma_floor_mag)?;
        writeln!(f, "  \"noise_floor_sync\": {{ \"orig\": {:.4}, \"deci\": {:.4} }},", orig_sync_floor_mag, deci_sync_floor_mag)?;
        writeln!(f, "  \"noise_floor_white\": {{ \"orig\": {:.4}, \"deci\": {:.4} }},", orig_white_floor_mag, deci_white_floor_mag)?;
        writeln!(f, "  \"noise_floor_chroma\": {{ \"orig\": {:.4}, \"deci\": {:.4} }},", orig_chroma_floor_mag, deci_chroma_floor_mag)?;
        writeln!(f, "  \"noise_floor_chroma_method\": {{ \"orig\": \"{}\", \"deci\": \"{}\" }},", orig_chroma_floor_method, deci_chroma_floor_method)?;
        writeln!(f, "  \"enob\": {{ \"orig\": {:.2}, \"deci\": {:.2} }},", orig_enob, deci_enob)?;
        writeln!(f, "  \"carrier_peaks\": {{")?;
        writeln!(f, "    \"luma_orig\": {{ \"mag_db\": {:.4}, \"freq_hz\": {:.2} }},", orig_luma_peak_mag, orig_luma_peak_freq)?;
        writeln!(f, "    \"luma_deci\": {{ \"mag_db\": {:.4}, \"freq_hz\": {:.2} }},", deci_luma_peak_mag, deci_luma_peak_freq)?;
        writeln!(f, "    \"sync_orig\": {{ \"mag_db\": {:.4}, \"freq_hz\": {:.2} }},", orig_sync_peak_mag, orig_sync_peak_freq)?;
        writeln!(f, "    \"sync_deci\": {{ \"mag_db\": {:.4}, \"freq_hz\": {:.2} }},", deci_sync_peak_mag, deci_sync_peak_freq)?;
        writeln!(f, "    \"white_orig\": {{ \"mag_db\": {:.4}, \"freq_hz\": {:.2} }},", orig_white_peak_mag, orig_white_peak_freq)?;
        writeln!(f, "    \"white_deci\": {{ \"mag_db\": {:.4}, \"freq_hz\": {:.2} }},", deci_white_peak_mag, deci_white_peak_freq)?;
        writeln!(f, "    \"chroma_orig\": {{ \"mag_db\": {:.4}, \"freq_hz\": {:.2} }},", orig_chroma_peak_mag, orig_chroma_peak_freq)?;
        writeln!(f, "    \"chroma_deci\": {{ \"mag_db\": {:.4}, \"freq_hz\": {:.2} }}", deci_chroma_peak_mag, deci_chroma_peak_freq)?;
        writeln!(f, "  }},")?;
        // PSD arrays for GUI graphing
        write!(f, "  \"psd_freq_hz\": [")?;
        for i in 0..orig.psd_dbfs_hz.len() {
            if i > 0 { write!(f, ",")?; }
            write!(f, "{:.1}", i as f64 * orig.fft_bin_hz)?;
        }
        writeln!(f, "],")?;
        write!(f, "  \"psd_deci_freq_hz\": [")?;
        for i in 0..deci.psd_dbfs_hz.len() {
            if i > 0 { write!(f, ",")?; }
            write!(f, "{:.1}", i as f64 * deci.fft_bin_hz)?;
        }
        writeln!(f, "],")?;
        write!(f, "  \"psd_orig_dbfs_hz\": [")?;
        for (i, v) in orig.psd_dbfs_hz.iter().enumerate() {
            if i > 0 { write!(f, ",")?; }
            write!(f, "{:.2}", v)?;
        }
        writeln!(f, "],")?;
        write!(f, "  \"psd_deci_dbfs_hz\": [")?;
        for (i, v) in deci.psd_dbfs_hz.iter().enumerate() {
            if i > 0 { write!(f, ",")?; }
            write!(f, "{:.2}", v)?;
        }
        writeln!(f, "],")?;
        write!(f, "  \"plot_orig_mag_db\": [")?;
        for (i, v) in orig.peak_mag_db.iter().enumerate() {
            if i > 0 { write!(f, ",")?; }
            write!(f, "{:.2}", v)?;
        }
        writeln!(f, "],")?;
        write!(f, "  \"plot_deci_mag_db\": [")?;
        for (i, v) in deci.peak_mag_db.iter().enumerate() {
            if i > 0 { write!(f, ",")?; }
            write!(f, "{:.2}", v)?;
        }
        writeln!(f, "],")?;
        // Bands for graph annotation
        writeln!(f, "  \"bands\": {{")?;
        writeln!(f, "    \"luma_signal\": [{:.0}, {:.0}],", preset.luma_signal.f_start_hz, preset.luma_signal.f_stop_hz)?;
        writeln!(f, "    \"luma_sync_ref_hz\": {:.0},", preset.luma_sync_hz)?;
        writeln!(f, "    \"luma_white_ref_hz\": {:.0},", preset.luma_white_hz)?;
        writeln!(f, "    \"chroma_signal\": [{:.0}, {:.0}],", preset.chroma_signal.f_start_hz, preset.chroma_signal.f_stop_hz)?;
        writeln!(f, "    \"luma_noise_low\": [{:.0}, {:.0}],", preset.luma_noise_low.f_start_hz, preset.luma_noise_low.f_stop_hz)?;
        writeln!(f, "    \"luma_noise_high\": [{:.0}, {:.0}],", preset.luma_noise_high.f_start_hz, preset.luma_noise_high.f_stop_hz)?;
        writeln!(f, "    \"chroma_noise_low\": [{:.0}, {:.0}],", preset.chroma_noise_low.f_start_hz, preset.chroma_noise_low.f_stop_hz)?;
        writeln!(f, "    \"chroma_noise_high\": [{:.0}, {:.0}]", preset.chroma_noise_high.f_start_hz, preset.chroma_noise_high.f_stop_hz)?;
        writeln!(f, "  }}")?;
        writeln!(f, "}}")?;
    }

    Ok(())
}

// ─── Main ────────────────────────────────────────────────────────────────────

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = parse_args()?;
    match &cfg.mode {
        RunMode::Calibrate { adc_file, chain_file, cal_output } => {
            run_calibrate(&cfg, adc_file, chain_file, cal_output)
        }
        RunMode::Compare { original, decimated, calibration, csv, psd_csv, json_out } => {
            run_compare(&cfg, original, decimated, calibration, csv, psd_csv, json_out)
        }
    }
}

// ─── Presets ─────────────────────────────────────────────────────

fn rf_preset(standard: &str) -> Result<RfPreset, Box<dyn std::error::Error>> {
    let s = standard.to_ascii_lowercase();
    match s.as_str() {
        "ntsc" => {
            let luma_sync = 3_400_000.0;
            let luma_white = 4_400_000.0;
            let chroma = 629_371.0;
            Ok(RfPreset {
                name: "NTSC", luma_sync_hz: luma_sync, luma_white_hz: luma_white, chroma_carrier_hz: chroma,
                luma_signal: Band { name: "luma_signal_band", f_start_hz: 3_300_000.0, f_stop_hz: 4_500_000.0 },
                luma_sync_band: Band { name: "luma_sync_region", f_start_hz: 3_300_000.0, f_stop_hz: 3_500_000.0 },
                luma_white_band: Band { name: "luma_white_region", f_start_hz: 4_300_000.0, f_stop_hz: 4_500_000.0 },
                luma_noise_low: Band { name: "luma_noise_low", f_start_hz: 2_000_000.0, f_stop_hz: 2_500_000.0 },
                luma_noise_high: Band { name: "luma_noise_high", f_start_hz: 5_500_000.0, f_stop_hz: 6_500_000.0 },
                chroma_signal: Band { name: "chroma_carrier", f_start_hz: 60_000.0, f_stop_hz: 1_300_000.0 },
                chroma_noise_low: Band { name: "chroma_noise_low", f_start_hz: 225_000.0, f_stop_hz: 400_000.0 },
                chroma_noise_high: Band { name: "chroma_noise_high", f_start_hz: 950_000.0, f_stop_hz: 1_300_000.0 },
            })
        }
        "pal" | "secam" => {
            let luma_sync = 3_800_000.0;
            let luma_white = 4_800_000.0;
            let chroma = 625_953.0;
            Ok(RfPreset {
                name: "PAL", luma_sync_hz: luma_sync, luma_white_hz: luma_white, chroma_carrier_hz: chroma,
                luma_signal: Band { name: "luma_signal_band", f_start_hz: 3_700_000.0, f_stop_hz: 4_900_000.0 },
                luma_sync_band: Band { name: "luma_sync_region", f_start_hz: 3_700_000.0, f_stop_hz: 3_900_000.0 },
                luma_white_band: Band { name: "luma_white_region", f_start_hz: 4_700_000.0, f_stop_hz: 4_900_000.0 },
                luma_noise_low: Band { name: "luma_noise_low", f_start_hz: 2_500_000.0, f_stop_hz: 3_000_000.0 },
                luma_noise_high: Band { name: "luma_noise_high", f_start_hz: 6_000_000.0, f_stop_hz: 7_000_000.0 },
                chroma_signal: Band { name: "chroma_carrier", f_start_hz: 60_000.0, f_stop_hz: 1_300_000.0 },
                chroma_noise_low: Band { name: "chroma_noise_low", f_start_hz: 225_000.0, f_stop_hz: 400_000.0 },
                chroma_noise_high: Band { name: "chroma_noise_high", f_start_hz: 950_000.0, f_stop_hz: 1_300_000.0 },
            })
        }
        "m-pal" | "mpal" => {
            let luma_sync = 3_400_000.0;
            let luma_white = 4_400_000.0;
            let chroma = 631_337.0;
            Ok(RfPreset {
                name: "M-PAL", luma_sync_hz: luma_sync, luma_white_hz: luma_white, chroma_carrier_hz: chroma,
                luma_signal: Band { name: "luma_signal_band", f_start_hz: 3_300_000.0, f_stop_hz: 4_500_000.0 },
                luma_sync_band: Band { name: "luma_sync_region", f_start_hz: 3_300_000.0, f_stop_hz: 3_500_000.0 },
                luma_white_band: Band { name: "luma_white_region", f_start_hz: 4_300_000.0, f_stop_hz: 4_500_000.0 },
                luma_noise_low: Band { name: "luma_noise_low", f_start_hz: 2_000_000.0, f_stop_hz: 2_500_000.0 },
                luma_noise_high: Band { name: "luma_noise_high", f_start_hz: 5_500_000.0, f_stop_hz: 6_500_000.0 },
                chroma_signal: Band { name: "chroma_carrier", f_start_hz: 60_000.0, f_stop_hz: 1_300_000.0 },
                chroma_noise_low: Band { name: "chroma_noise_low", f_start_hz: 225_000.0, f_stop_hz: 400_000.0 },
                chroma_noise_high: Band { name: "chroma_noise_high", f_start_hz: 950_000.0, f_stop_hz: 1_300_000.0 },
            })
        }
        "n-pal" | "npal" => {
            let luma_sync = 3_800_000.0;
            let luma_white = 4_800_000.0;
            let chroma = 626_953.0;
            Ok(RfPreset {
                name: "N-PAL", luma_sync_hz: luma_sync, luma_white_hz: luma_white, chroma_carrier_hz: chroma,
                luma_signal: Band { name: "luma_signal_band", f_start_hz: 3_700_000.0, f_stop_hz: 4_900_000.0 },
                luma_sync_band: Band { name: "luma_sync_region", f_start_hz: 3_700_000.0, f_stop_hz: 3_900_000.0 },
                luma_white_band: Band { name: "luma_white_region", f_start_hz: 4_700_000.0, f_stop_hz: 4_900_000.0 },
                luma_noise_low: Band { name: "luma_noise_low", f_start_hz: 2_500_000.0, f_stop_hz: 3_000_000.0 },
                luma_noise_high: Band { name: "luma_noise_high", f_start_hz: 6_000_000.0, f_stop_hz: 7_000_000.0 },
                chroma_signal: Band { name: "chroma_carrier", f_start_hz: 60_000.0, f_stop_hz: 1_300_000.0 },
                chroma_noise_low: Band { name: "chroma_noise_low", f_start_hz: 225_000.0, f_stop_hz: 400_000.0 },
                chroma_noise_high: Band { name: "chroma_noise_high", f_start_hz: 950_000.0, f_stop_hz: 1_300_000.0 },
            })
        }
        _ => Err(format!("unknown standard '{}', expected NTSC, PAL, M-PAL, or N-PAL", standard).into()),
    }
}

// ─── Argument parsing ────────────────────────────────────────────────────────

fn parse_args() -> Result<Config, Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    let mut standard = "ntsc".to_string();
    let mut window = "blackman".to_string();
    let mut orig_rate = 40_000_000u32;
    let mut deci_rate = 20_000_000u32;
    let mut orig_bits: Option<u32> = None;
    let mut deci_bits: Option<u32> = None;

    // Check for calibrate mode
    if args.iter().any(|a| a == "--calibrate") {
        let mut adc_file = None;
        let mut chain_file = None;
        let mut cal_output = None;
        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--calibrate" => {}
                "--adc-file" => { i += 1; adc_file = Some(PathBuf::from(&args[i])); }
                "--chain-file" => { i += 1; chain_file = Some(PathBuf::from(&args[i])); }
                "--cal-output" => { i += 1; cal_output = Some(PathBuf::from(&args[i])); }
                "--standard" => { i += 1; standard = args[i].clone(); }
                "--window" => {
                    i += 1;
                    let w = args[i].to_ascii_lowercase();
                    if w != "blackman" {
                        return Err("only Blackman-Harris is supported: use --window blackman".into());
                    }
                    window = w;
                }
                "--orig-rate" => { i += 1; orig_rate = args[i].parse()?; }
                "--orig-bits" => { i += 1; orig_bits = Some(args[i].parse()?); }
                _ => {}
            }
            i += 1;
        }
        return Ok(Config {
            mode: RunMode::Calibrate {
                adc_file: adc_file.ok_or("--calibrate requires --adc-file")?,
                chain_file: chain_file.ok_or("--calibrate requires --chain-file")?,
                cal_output: cal_output.ok_or("--calibrate requires --cal-output")?,
            },
            standard, window, orig_rate, deci_rate, orig_bits, deci_bits,
        });
    }

    // Compare mode
    if args.len() < 3 {
        return Err("Usage: compare-rf <original> <decimated> [options]\n  --standard ntsc|pal|m-pal|n-pal\n  --calibration file.cal.json\n  --csv metrics.csv\n  --psd-csv prefix\n  --json results.json\n  --orig-bits N  --deci-bits N\n  --orig-rate N  --deci-rate N\n  --window blackman\n\nCalibrate mode:\n  compare-rf --calibrate --adc-file X --chain-file Y --cal-output Z.cal.json [--standard ...]\n".into());
    }

    let original = PathBuf::from(&args[1]);
    let decimated = PathBuf::from(&args[2]);
    let mut calibration = None;
    let mut csv = None;
    let mut psd_csv = None;
    let mut json_out = None;

    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--standard" => { i += 1; standard = args[i].clone(); }
            "--calibration" => { i += 1; calibration = Some(PathBuf::from(&args[i])); }
            "--csv" => { i += 1; csv = Some(PathBuf::from(&args[i])); }
            "--psd-csv" => { i += 1; psd_csv = Some(PathBuf::from(&args[i])); }
            "--json" => { i += 1; json_out = Some(PathBuf::from(&args[i])); }
            "--orig-bits" => { i += 1; orig_bits = Some(args[i].parse()?); }
            "--deci-bits" => { i += 1; deci_bits = Some(args[i].parse()?); }
            "--orig-rate" => { i += 1; orig_rate = args[i].parse()?; }
            "--deci-rate" => { i += 1; deci_rate = args[i].parse()?; }
            "--window" => {
                i += 1;
                let w = args[i].to_ascii_lowercase();
                if w != "blackman" {
                    return Err("only Blackman-Harris is supported: use --window blackman".into());
                }
                window = w;
            }
            // Legacy compat: silently ignore old baseline args
            "--baseline-adc" | "--baseline-chain" => { i += 1; }
            "--help" | "-h" => {
                println!("Usage: compare-rf <original> <decimated> [options]");
                println!("  --standard ntsc|pal|m-pal|n-pal");
                println!("  --calibration file.cal.json");
                println!("  --csv metrics.csv  --psd-csv prefix  --json results.json");
                println!("  --orig-bits N  --deci-bits N  --orig-rate N  --deci-rate N");
                println!("  --window blackman");
                println!("\nCalibrate: compare-rf --calibrate --adc-file X --chain-file Y --cal-output Z.cal.json");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}").into()),
        }
        i += 1;
    }

    Ok(Config {
        mode: RunMode::Compare { original, decimated, calibration, csv, psd_csv, json_out },
        standard, window, orig_rate, deci_rate, orig_bits, deci_bits,
    })
}

// ─── File readers (same perf-optimized versions) ─────────────────────────────

fn analyze_file(path: &Path, sample_rate: u32, bits_hint: Option<u32>, window: &str) -> Result<Metrics, Box<dyn std::error::Error>> {
    if !path.exists() {
        return Err(format!("file not found: {}", path.display()).into());
    }
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
    if ext == "flac" { analyze_flac(path, sample_rate, window) }
    else { analyze_raw(path, sample_rate, bits_hint.unwrap_or(16), &ext, window) }
}

fn analyze_flac(path: &Path, sample_rate_hint: u32, window: &str) -> Result<Metrics, Box<dyn std::error::Error>> {
    let file = File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("flac");
    let probed = symphonia::default::get_probe().format(
        &hint, mss,
        &FormatOptions { enable_gapless: false, ..Default::default() },
        &MetadataOptions::default(),
    )?;
    let mut format = probed.format;
    let track = format.default_track().ok_or("no audio track")?.clone();
    let track_id = track.id;
    let bits = track.codec_params.bits_per_sample.unwrap_or(16);
    let shift = 32u32.saturating_sub(bits);

    // Use FLAC header sample rate (authoritative); warn if hint differs
    // FLAC spec max is 655350 Hz, so RF captures at 40 MSPS store 40000 in header.
    // Interpret values <= 655350 and >= 1000 as kSPS (multiply by 1000 to get Hz).
    let mut flac_rate = track.codec_params.sample_rate.unwrap_or(sample_rate_hint);
    if flac_rate <= 655350 && flac_rate >= 1000 {
        flac_rate *= 1000;
    }
    if flac_rate != sample_rate_hint && sample_rate_hint != 40_000_000 && sample_rate_hint != 20_000_000 {
        eprintln!("WARNING: FLAC header says {} Hz but --orig-rate/--deci-rate says {} Hz; using FLAC header", flac_rate, sample_rate_hint);
    } else if flac_rate != sample_rate_hint {
        eprintln!("NOTE: Using sample rate from FLAC header: {} Hz", flac_rate);
    }
    let sample_rate = flac_rate;

    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;
    let mut sample_buf: Option<SampleBuffer<i32>> = None;
    let mut acc = Accum::new(sample_rate, bits, window);

    loop {
        let packet = match format.next_packet() { Ok(p) => p, Err(_) => break };
        if packet.track_id() != track_id { continue; }
        let decoded = match decoder.decode(&packet) { Ok(d) => d, Err(_) => continue };
        let spec = *decoded.spec();
        let duration = decoded.capacity();
        let buf = sample_buf.get_or_insert_with(|| SampleBuffer::<i32>::new(duration as u64, spec));
        if buf.capacity() < duration { *buf = SampleBuffer::<i32>::new(duration as u64, spec); }
        buf.copy_interleaved_ref(decoded);
        let samples = buf.samples();
        let mut f32_buf: Vec<f32> = Vec::with_capacity(samples.len());
        for &s in samples { f32_buf.push((s >> shift) as f32); }
        acc.push_batch(&f32_buf);
    }
    Ok(acc.finalize())
}

fn analyze_raw(path: &Path, sample_rate: u32, bits: u32, ext: &str, window: &str) -> Result<Metrics, Box<dyn std::error::Error>> {
    if bits == 0 || bits > 16 { return Err("raw bit depth must be in 1..=16".into()); }
    let mut acc = Accum::new(sample_rate, bits, window);

    if ext == "8bit" {
        let mut reader = BufReader::with_capacity(1 << 22, File::open(path)?);
        let mut buf = vec![0u8; 1 << 20];
        let mut f32_buf = vec![0.0f32; 1 << 20];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 { break; }
            for i in 0..n { f32_buf[i] = (buf[i] as i8) as f32; }
            acc.push_batch(&f32_buf[..n]);
        }
        return Ok(acc.finalize());
    }

    let mut reader = BufReader::with_capacity(1 << 22, File::open(path)?);
    let mut buf = vec![0u8; 1 << 21];
    let mut f32_buf = vec![0.0f32; 1 << 20];
    let midpoint = (1u32 << (bits - 1)) as f32;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 { break; }
        if n % 2 != 0 { return Err(format!("file size is not even: {}", path.display()).into()); }
        let pairs = n / 2;
        for i in 0..pairs {
            let lo = buf[i * 2];
            let hi = buf[i * 2 + 1];
            f32_buf[i] = if ext == "s16" {
                i16::from_le_bytes([lo, hi]) as f32
            } else {
                let u = u16::from_le_bytes([lo, hi]) as f32;
                if bits < 16 { u - midpoint } else { u - 32768.0 }
            };
        }
        acc.push_batch(&f32_buf[..pairs]);
    }
    Ok(acc.finalize())
}

// ─── Utilities ───────────────────────────────────────────────────────────────

fn format_with_commas(v: u64) -> String {
    let s = v.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 { out.push(','); }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn format_duration_hms(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}
