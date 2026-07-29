#!/usr/bin/env python3
"""
VHS RF Signal Capture Analyser v6.5
Tkinter interface for comparing original and comparison RF signal files.
NSD/dBFS-based SNR analysis with calibration support and PSD graphing.
Original use-case was for comparing Original vs. Decimated capture improvement
Therefore 'decimated' was used, has been worded as "comparison" in text boxes,
for second/comparison file as tool can compare Before and After modification of
processing or hardware, irrespective of bitness or sample rate.
"""

import sys
import os
import json
import threading
import tempfile
from pathlib import Path
from tkinter import Tk, Frame, Label, Button, Entry, StringVar, IntVar, Text, Scrollbar
from tkinter import filedialog, messagebox, ttk
from tkinter.ttk import Combobox
import subprocess
import sys

# Suppress console window on Windows when launching backend
_SUBPROCESS_FLAGS = 0
if sys.platform == 'win32':
    _SUBPROCESS_FLAGS = subprocess.CREATE_NO_WINDOW
import re

try:
    import matplotlib
    matplotlib.use('TkAgg')
    import matplotlib.pyplot as plt
    HAS_MATPLOTLIB = True
except ImportError:
    HAS_MATPLOTLIB = False


class SignalCompareGUI:
    APP_NAME = "VHS RF Signal Analyser"
    try:
        _base = Path(getattr(sys, '_MEIPASS', Path(__file__).parent))
        _version_file = _base / "VERSION"
        if not _version_file.exists():
            _version_file = Path(__file__).parent / "VERSION"
        APP_VERSION = _version_file.read_text().strip() if _version_file.exists() else "0.0"
    except Exception:
        APP_VERSION = "0.0"

    def __init__(self, root):
        self.root = root
        self.root.title(f"{self.APP_NAME} v{self.APP_VERSION}")
        self.root.geometry("1100x850")
        self.root.resizable(True, True)
        self.root.minsize(800, 600)
        
        # AppData path for persistence
        self.appdata_dir = Path(os.environ.get('APPDATA', '.')) / 'VHS-RF-Analyse'
        self.appdata_dir.mkdir(parents=True, exist_ok=True)
        self.config_file = self.appdata_dir / 'last_paths.json'
        
        # Load last used paths
        last_paths = self._load_config()
        
        self.original_file = StringVar(value=last_paths.get('original', ''))
        self.decimated_file = StringVar(value=last_paths.get('decimated', ''))
        self.baseline_adc_file = StringVar(value=last_paths.get('baseline_adc', ''))
        self.baseline_chain_file = StringVar(value=last_paths.get('baseline_chain', ''))
        self.calibration_file = StringVar(value=last_paths.get('calibration', ''))
        self.orig_bits = StringVar(value="auto")
        self.deci_bits = StringVar(value="auto")
        self.orig_rate = StringVar(value="40")
        self.deci_rate = StringVar(value="40")
        # Window is fixed to Blackman-Harris in the backend.
        self.window_func = StringVar(value="blackman")
        saved_standard = last_paths.get('standard', 'NTSC')
        if saved_standard == 'PAL/SECAM':
            saved_standard = 'PAL'
        self.rf_standard = StringVar(value=saved_standard)
        self.save_csv = IntVar(value=1)
        self.csv_file = StringVar()
        
        self.is_running = False
        self._open_figures = []
        self._temp_files = []
        
        self._build_gui()
        
        # Save paths when window closes
        self.root.protocol("WM_DELETE_WINDOW", self._on_closing)
    
    def _load_config(self) -> dict:
        """Load last used paths from AppData."""
        if self.config_file.exists():
            try:
                with open(self.config_file, 'r') as f:
                    return json.load(f)
            except:
                return {}
        return {}
    
    def _save_config(self):
        """Save last used paths to AppData."""
        config = {
            'original': self.original_file.get(),
            'decimated': self.decimated_file.get(),
            'baseline_adc': self.baseline_adc_file.get(),
            'baseline_chain': self.baseline_chain_file.get(),
            'calibration': self.calibration_file.get(),
            'standard': self.rf_standard.get(),
        }
        try:
            with open(self.config_file, 'w') as f:
                json.dump(config, f, indent=2)
        except Exception as e:
            print(f"Warning: Could not save config: {e}", file=sys.stderr)
    
    def _on_closing(self):
        """Handle window close event."""
        self._save_config()
        # Clean up temp files
        for tmp in self._temp_files:
            try:
                p = Path(tmp)
                if p.exists():
                    p.unlink()
            except Exception:
                pass
        self._temp_files.clear()
        self.root.destroy()
    
    def _build_gui(self):
        """Construct the GUI layout."""
        
        # Title
        title = Label(self.root, text=f"{self.APP_NAME} v{self.APP_VERSION}",
                      font=("Arial", 12, "bold"))
        title.pack(pady=4)
        
        # File Selection Frame
        file_frame = Frame(self.root, relief="ridge", borderwidth=2)
        file_frame.pack(fill="x", padx=10, pady=3)
        
        Label(file_frame, text="FILE SELECTION", font=("Arial", 10, "bold")).pack(anchor="w", padx=5, pady=2)
        
        # Original file
        orig_inner = Frame(file_frame)
        orig_inner.pack(fill="x", padx=10, pady=2)
        Label(orig_inner, text="Original RF Capture:", width=20, anchor="w").pack(side="left")
        Entry(orig_inner, textvariable=self.original_file, width=50).pack(side="left", padx=5)
        Button(orig_inner, text="Browse...", command=self._browse_original, width=10).pack(side="left")
        
        # Decimated file
        deci_inner = Frame(file_frame)
        deci_inner.pack(fill="x", padx=10, pady=2)
        Label(deci_inner, text="Comparison RF Capture:", width=20, anchor="w").pack(side="left")
        Entry(deci_inner, textvariable=self.decimated_file, width=50).pack(side="left", padx=5)
        Button(deci_inner, text="Browse...", command=self._browse_decimated, width=10).pack(side="left")

        # Baseline ADC noise file
        base_adc_inner = Frame(file_frame)
        base_adc_inner.pack(fill="x", padx=10, pady=2)
        Label(base_adc_inner, text="Baseline ADC Noise:", width=20, anchor="w").pack(side="left")
        Entry(base_adc_inner, textvariable=self.baseline_adc_file, width=50).pack(side="left", padx=5)
        Button(base_adc_inner, text="Browse...", command=self._browse_baseline_adc, width=10).pack(side="left")

        # Baseline chain noise file
        base_chain_inner = Frame(file_frame)
        base_chain_inner.pack(fill="x", padx=10, pady=2)
        Label(base_chain_inner, text="Baseline Chain Noise:", width=20, anchor="w").pack(side="left")
        Entry(base_chain_inner, textvariable=self.baseline_chain_file, width=50).pack(side="left", padx=5)
        Button(base_chain_inner, text="Browse...", command=self._browse_baseline_chain, width=10).pack(side="left")

        # Calibration file
        cal_inner = Frame(file_frame)
        cal_inner.pack(fill="x", padx=10, pady=2)
        Label(cal_inner, text="Calibration File:", width=20, anchor="w").pack(side="left")
        Entry(cal_inner, textvariable=self.calibration_file, width=50).pack(side="left", padx=5)
        Button(cal_inner, text="Browse...", command=self._browse_calibration, width=10).pack(side="left")
        
        # Configuration Frame
        config_frame = Frame(self.root, relief="ridge", borderwidth=2)
        config_frame.pack(fill="x", padx=10, pady=3)
        
        Label(config_frame, text="CONFIGURATION", font=("Arial", 10, "bold")).pack(anchor="w", padx=5, pady=2)
        
        # Bit depths
        bits_frame = Frame(config_frame)
        bits_frame.pack(fill="x", padx=10, pady=5)
        Label(bits_frame, text="Bit Depth:", width=15, anchor="w").pack(side="left")
        Label(bits_frame, text="Original:", width=10, anchor="w").pack(side="left", padx=(20, 0))
        Combobox(bits_frame, textvariable=self.orig_bits, values=["auto", "8", "12", "16"], 
                 width=6, state="readonly").pack(side="left", padx=2)
        Label(bits_frame, text="Comparison:", width=12, anchor="w").pack(side="left", padx=(20, 0))
        Combobox(bits_frame, textvariable=self.deci_bits, values=["auto", "8", "12", "16"], 
                 width=6, state="readonly").pack(side="left", padx=2)
        
        # Sample rates
        self.rate_frame = Frame(config_frame)
        self.rate_frame.pack(fill="x", padx=10, pady=5)
        Label(self.rate_frame, text="Sample Rate (MSPS):", width=18, anchor="w").pack(side="left")
        Label(self.rate_frame, text="Original:", width=10, anchor="w").pack(side="left", padx=(20, 0))
        Entry(self.rate_frame, textvariable=self.orig_rate, width=12).pack(side="left", padx=2)
        Label(self.rate_frame, text="Comparison:", width=12, anchor="w").pack(side="left", padx=(20, 0))
        Entry(self.rate_frame, textvariable=self.deci_rate, width=12).pack(side="left", padx=2)
        
        # Window function is fixed to Blackman-Harris.
        window_frame = Frame(config_frame)
        window_frame.pack(fill="x", padx=10, pady=5)
        Label(window_frame, text="FFT Window:", width=15, anchor="w").pack(side="left")
        Label(window_frame, text="Blackman-Harris", anchor="w").pack(side="left", padx=2)

        # RF standard
        std_frame = Frame(config_frame)
        std_frame.pack(fill="x", padx=10, pady=5)
        Label(std_frame, text="RF Standard:", width=15, anchor="w").pack(side="left")
        Combobox(std_frame, textvariable=self.rf_standard,
                 values=["NTSC", "PAL", "M-PAL", "N-PAL"], width=15, state="readonly").pack(side="left", padx=2)
        
        # CSV output
        csv_frame = Frame(config_frame)
        csv_frame.pack(fill="x", padx=10, pady=5)
        from tkinter import Checkbutton
        Checkbutton(csv_frame, text="Save results to CSV", variable=self.save_csv).pack(side="left")
        Entry(csv_frame, textvariable=self.csv_file, width=40).pack(side="left", padx=10)
        Button(csv_frame, text="Browse...", command=self._browse_csv, width=10).pack(side="left")
        
        # Button Frame (before results so always visible)
        button_frame = Frame(self.root)
        button_frame.pack(fill="x", padx=10, pady=4)
        
        self.run_button = Button(button_frame, text="COMPARE", command=self._run_analysis, 
                                  bg="#4CAF50", fg="white", font=("Arial", 10, "bold"), 
                                  width=14, height=1)
        self.run_button.pack(side="left", padx=5)

        self.cal_button = Button(button_frame, text="ADC-BASELINE", command=self._run_calibration,
                                  bg="#2196F3", fg="white", font=("Arial", 10, "bold"),
                                  width=14, height=1)
        self.cal_button.pack(side="left", padx=5)
        
        Button(button_frame, text="Clear", command=self._clear_results, width=12).pack(side="left", padx=5)
        Button(button_frame, text="Exit", command=self.root.quit, width=12).pack(side="left", padx=5)

        # Progress bar
        self.progress = ttk.Progressbar(self.root, mode='indeterminate')
        self.progress.pack(fill="x", padx=10, pady=2)
        
        # Status label
        self.status_label = Label(self.root, text="Ready", fg="black")
        self.status_label.pack(anchor="w", padx=10)

        # Results Frame
        results_frame = Frame(self.root, relief="ridge", borderwidth=2)
        results_frame.pack(fill="both", expand=True, padx=10, pady=3)
        
        Label(results_frame, text="RESULTS", font=("Arial", 10, "bold")).pack(anchor="w", padx=5, pady=2)
        
        # Text widget with scrollbar
        scrollbar = Scrollbar(results_frame)
        scrollbar.pack(side="right", fill="y")
        
        self.results_text = Text(results_frame, height=10, width=100, 
                                 yscrollcommand=scrollbar.set, font=("Courier", 9))
        self.results_text.pack(fill="both", expand=True, padx=5, pady=3)
        scrollbar.config(command=self.results_text.yview)
    
    def _browse_original(self):
        """Browse for original file."""
        initial_dir = self._initial_dir_for(self.original_file.get())
        path = filedialog.askopenfilename(
            title="Select original RF file",
            initialdir=initial_dir,
            filetypes=[
                ("RF Signal Files", "*.u16 *.s16 *.u8 *.flac"),
                ("16-bit Raw", "*.u16 *.s16"),
                ("8-bit Raw", "*.u8"),
                ("FLAC Audio", "*.flac"),
                ("All files", "*.*")
            ]
        )
        if path:
            self.original_file.set(path)
            self._save_config()
            self._update_rate_visibility()
    
    def _browse_decimated(self):
        """Browse for file to Compare."""
        initial_dir = self._initial_dir_for(self.decimated_file.get())
        path = filedialog.askopenfilename(
            title="Select Comparison RF Capture",
            initialdir=initial_dir,
            filetypes=[
                ("RF Signal Files", "*.u16 *.s16 *.u8 *.flac"),
                ("16-bit Raw", "*.u16 *.s16"),
                ("8-bit Raw", "*.u8"),
                ("FLAC Audio", "*.flac"),
                ("All files", "*.*")
            ]
        )
        if path:
            self.decimated_file.set(path)
            self._save_config()
            self._update_rate_visibility()

    def _browse_baseline_adc(self):
        """Browse for ADC-only noise profile file."""
        initial_dir = self._initial_dir_for(self.baseline_adc_file.get())
        path = filedialog.askopenfilename(
            title="Select ADC-only noise baseline file",
            initialdir=initial_dir,
            filetypes=[
                ("RF Signal Files", "*.u16 *.s16 *.u8 *.flac"),
                ("16-bit Raw", "*.u16 *.s16"),
                ("8-bit Raw", "*.u8"),
                ("FLAC Audio", "*.flac"),
                ("All files", "*.*")
            ]
        )
        if path:
            self.baseline_adc_file.set(path)
            self._save_config()

    def _browse_baseline_chain(self):
        """Browse for chain-noise baseline file."""
        initial_dir = self._initial_dir_for(self.baseline_chain_file.get())
        path = filedialog.askopenfilename(
            title="Select chain-noise baseline file (VCR+amp+ADC)",
            initialdir=initial_dir,
            filetypes=[
                ("RF Signal Files", "*.u16 *.s16 *.u8 *.flac"),
                ("16-bit Raw", "*.u16 *.s16"),
                ("8-bit Raw", "*.u8"),
                ("FLAC Audio", "*.flac"),
                ("All files", "*.*")
            ]
        )
        if path:
            self.baseline_chain_file.set(path)
            self._save_config()

    def _browse_calibration(self):
        """Browse for ADC Baseline-Profile file."""
        initial_dir = self._initial_dir_for(self.calibration_file.get())
        path = filedialog.askopenfilename(
            title="Select baseline file",
            initialdir=initial_dir,
            filetypes=[("Calibration JSON", "*.cal.json"), ("All files", "*.*")]
        )
        if path:
            self.calibration_file.set(path)
            self._save_config()
    
    def _browse_csv(self):
        """Browse for CSV output file."""
        path = filedialog.asksaveasfilename(
            title="Save results as CSV",
            defaultextension=".csv",
            filetypes=[("CSV", "*.csv"), ("All files", "*.*")]
        )
        if path:
            self.csv_file.set(path)
    
    def _update_rate_visibility(self):
        """Auto-populate sample rate fields from FLAC headers when FLAC files are selected.
        FLAC spec max is 655350 Hz, so RF captures at 40 MSPS are often stored as 40000.
        We interpret values <= 655350 as kSPS and multiply by 1000 to get Hz."""
        orig = self.original_file.get()
        deci = self.decimated_file.get()
        if orig.lower().endswith('.flac'):
            rate = self._get_flac_sample_rate(orig)
            if rate:
                rate = self._interpret_flac_rate(rate)
                self.orig_rate.set(str(rate // 1_000_000) if rate >= 1_000_000 else str(rate))
        if deci.lower().endswith('.flac'):
            rate = self._get_flac_sample_rate(deci)
            if rate:
                rate = self._interpret_flac_rate(rate)
                self.deci_rate.set(str(rate // 1_000_000) if rate >= 1_000_000 else str(rate))

    @staticmethod
    def _interpret_flac_rate(rate):
        """FLAC stores RF sample rates as kSPS (e.g. 40000 for 40 MSPS) because
        the FLAC spec maximum is 655350 Hz. Convert to actual Hz."""
        if rate <= 655350 and rate >= 1000:
            return rate * 1000
        return rate

    def _get_flac_sample_rate(self, path):
        """Read sample rate from a FLAC file header using mutagen or fallback."""
        try:
            import mutagen.flac
            f = mutagen.flac.FLAC(path)
            return f.info.sample_rate
        except Exception:
            pass
        # Fallback: read FLAC streaminfo block directly (first 42 bytes)
        try:
            with open(path, 'rb') as fh:
                header = fh.read(42)
                if header[:4] == b'fLaC':
                    # Sample rate is at bytes 18-20 (bits 80-99 of STREAMINFO)
                    sr = (header[18] << 12) | (header[19] << 4) | (header[20] >> 4)
                    if sr > 0:
                        return sr
        except Exception:
            pass
        return None

    def _rate_to_hz(self, msps_str):
        """Convert GUI rate field (MSPS or Hz) to integer Hz for backend.
        Accepts: '40' (MSPS), '40000000' (Hz), '20' (MSPS), etc."""
        val = float(msps_str.strip())
        if val < 1000:
            # Treat as MSPS
            return int(val * 1_000_000)
        else:
            # Already in Hz
            return int(val)

    def _clear_results(self):
        """Clear the results display."""
        self.results_text.config(state="normal")
        self.results_text.delete("1.0", "end")
        self.results_text.config(state="disabled")
        self.status_label.config(text="Ready", fg="black")
    
    def _run_analysis(self):
        """Run the signal comparison analysis."""
        if not self.original_file.get() or not self.decimated_file.get():
            messagebox.showerror("Error", "Please select both original and comparison RF files")
            return
        
        if self.is_running:
            messagebox.showwarning("Warning", "Analysis already in progress")
            return
        
        # Start analysis in background thread
        thread = threading.Thread(target=self._analysis_worker)
        thread.daemon = True
        thread.start()
    
    def _analysis_worker(self):
        """Background worker for signal analysis."""
        self.is_running = True
        self.run_button.config(state="disabled")
        self.cal_button.config(state="disabled")
        self.progress.start()
        self.status_label.config(text="Analyzing...", fg="orange")
        self.root.update()
        
        json_tmpfile = None
        try:
            cmd = self._find_backend_command() + [
                self.original_file.get(),
                self.decimated_file.get(),
            ]
            
            if self.orig_bits.get() != "auto":
                cmd.extend(["--orig-bits", self.orig_bits.get()])
            if self.deci_bits.get() != "auto":
                cmd.extend(["--deci-bits", self.deci_bits.get()])
            
            try:
                orig_rate = self._rate_to_hz(self.orig_rate.get())
                deci_rate = self._rate_to_hz(self.deci_rate.get())
                cmd.extend(["--orig-rate", str(orig_rate), "--deci-rate", str(deci_rate)])
            except ValueError:
                raise ValueError("Sample rates must be numeric (MSPS or Hz)")
            
            # Keep CLI flag for compatibility with older binaries.
            cmd.extend(["--window", "blackman"])
            cmd.extend(["--standard", self.rf_standard.get().lower()])

            # Calibration file
            cal_path = self.calibration_file.get().strip()
            if cal_path and Path(cal_path).exists():
                cmd.extend(["--calibration", cal_path])
            
            # JSON output for graphs
            json_tmpfile = tempfile.NamedTemporaryFile(suffix='.json', delete=False, prefix='compare_rf_')
            json_tmpfile.close()
            self._temp_files.append(json_tmpfile.name)
            cmd.extend(["--json", json_tmpfile.name])

            csv_path = None
            if self.save_csv.get():
                if self.csv_file.get():
                    csv_path = self.csv_file.get()
                else:
                    csv_path = str(Path(self.original_file.get()).stem) + "_compare.csv"
                cmd.extend(["--csv", csv_path])
            
            self._save_config()
            result = subprocess.run(cmd, capture_output=True, text=True, timeout=600, creationflags=_SUBPROCESS_FLAGS)
            
            self.results_text.config(state="normal")
            self.results_text.delete("1.0", "end")
            
            if result.returncode == 0:
                self.results_text.insert("end", result.stdout)
                if json_tmpfile:
                    self.results_text.insert("end", f"\nJSON graph data: {json_tmpfile.name}\n")
                if result.stderr.strip():
                    self.results_text.insert("end", "\n--- Log ---\n" + result.stderr)
                self.status_label.config(text="Analysis complete", fg="green")
                
                # Show graphs if matplotlib available and JSON was produced
                if HAS_MATPLOTLIB and json_tmpfile and Path(json_tmpfile.name).exists():
                    try:
                        with open(json_tmpfile.name, 'r') as f:
                            graph_data = json.load(f)
                        self.root.after(0, self._show_graphs, graph_data)
                    except Exception as ge:
                        self.results_text.insert("end", f"\n\nGraph error: {ge}")
                elif not HAS_MATPLOTLIB:
                    self.results_text.insert("end", "\n\nPlotting unavailable: matplotlib is not installed in the packaged environment.")
                
                if csv_path and Path(csv_path).exists():
                    messagebox.showinfo("Success", f"Results saved to:\n{csv_path}")
            else:
                self.results_text.insert("end", "ERROR:\n\n" + result.stderr + "\n" + result.stdout)
                self.status_label.config(text="Analysis failed", fg="red")
            
            self.results_text.config(state="disabled")
        
        except Exception as e:
            self.results_text.config(state="normal")
            self.results_text.delete("1.0", "end")
            self.results_text.insert("end", f"Error: {str(e)}")
            self.results_text.config(state="disabled")
            self.status_label.config(text="Error", fg="red")
        
        finally:
            self.progress.stop()
            self.run_button.config(state="normal")
            self.cal_button.config(state="normal")
            self.is_running = False
            self.root.update()

    def _run_calibration(self):
        """Run calibration from ADC + chain baseline files."""
        adc = self.baseline_adc_file.get().strip()
        chain = self.baseline_chain_file.get().strip()
        if not adc or not chain:
            messagebox.showerror("Error", "Calibration requires both ADC-only and Chain baseline files")
            return
        
        cal_output = filedialog.asksaveasfilename(
            title="Save calibration file",
            defaultextension=".cal.json",
            filetypes=[("Calibration JSON", "*.cal.json"), ("All files", "*.*")]
        )
        if not cal_output:
            return
        
        thread = threading.Thread(target=self._calibration_worker, args=(adc, chain, cal_output))
        thread.daemon = True
        thread.start()

    def _calibration_worker(self, adc_file, chain_file, cal_output):
        """Background worker for calibration."""
        self.is_running = True
        self.run_button.config(state="disabled")
        self.cal_button.config(state="disabled")
        self.progress.start()
        self.status_label.config(text="Calibrating...", fg="orange")
        self.root.update()
        
        try:
            cmd = self._find_backend_command() + [
                "--calibrate",
                "--adc-file", adc_file,
                "--chain-file", chain_file,
                "--cal-output", cal_output,
                "--standard", self.rf_standard.get().lower(),
            ]
            if self.orig_bits.get() != "auto":
                cmd.extend(["--orig-bits", self.orig_bits.get()])
            try:
                cmd.extend(["--orig-rate", str(self._rate_to_hz(self.orig_rate.get()))])
            except ValueError:
                pass

            result = subprocess.run(cmd, capture_output=True, text=True, timeout=600, creationflags=_SUBPROCESS_FLAGS)
            
            self.results_text.config(state="normal")
            self.results_text.delete("1.0", "end")
            
            if result.returncode == 0:
                self.results_text.insert("end", "Calibration complete!\n\n")
                self.results_text.insert("end", result.stderr)
                self.results_text.insert("end", "\n" + result.stdout)
                self.calibration_file.set(cal_output)
                self._save_config()
                self.status_label.config(text=f"Calibration saved: {cal_output}", fg="green")
            else:
                self.results_text.insert("end", "Calibration FAILED:\n\n" + result.stderr)
                self.status_label.config(text="Calibration failed", fg="red")
            
            self.results_text.config(state="disabled")
        except Exception as e:
            self.results_text.config(state="normal")
            self.results_text.delete("1.0", "end")
            self.results_text.insert("end", f"Calibration error: {str(e)}")
            self.results_text.config(state="disabled")
            self.status_label.config(text="Error", fg="red")
        finally:
            self.progress.stop()
            self.run_button.config(state="normal")
            self.cal_button.config(state="normal")
            self.is_running = False
            self.root.update()

    def _show_graphs(self, data):
        """Show measured PSD plots with explicit luma/chroma overlays and peak markers."""
        fig, axes = plt.subplots(3, 1, figsize=(15, 11), constrained_layout=True)
        self._open_figures.append(fig)

        def _release_figure(_event):
            try:
                self._open_figures.remove(fig)
            except ValueError:
                pass

        fig.canvas.mpl_connect('close_event', _release_figure)
        fig.suptitle(f"{self.APP_NAME} v{self.APP_VERSION} - {data.get('standard', 'Unknown')}", fontsize=13)

        freqs_hz = data.get('psd_freq_hz', [])
        deci_freqs_hz = data.get('psd_deci_freq_hz', [])
        orig_psd = data.get('plot_orig_mag_db') or data.get('psd_orig_dbfs_hz', [])
        deci_psd = data.get('plot_deci_mag_db') or data.get('psd_deci_dbfs_hz', [])
        # Extract actual rates from JSON metadata, fallback to GUI values if missing
        try:
            orig_rate = int(data.get('orig', {}).get('rate', 0)) or self._rate_to_hz(self.orig_rate.get())
            deci_rate = int(data.get('deci', {}).get('rate', 0)) or self._rate_to_hz(self.deci_rate.get())
        except (ValueError, TypeError):
            orig_rate = self._rate_to_hz(self.orig_rate.get())
            deci_rate = self._rate_to_hz(self.deci_rate.get())
        freqs_mhz = [f / 1e6 for f in freqs_hz] if freqs_hz and len(freqs_hz) == len(orig_psd) else [i * orig_rate / (2 * len(orig_psd)) / 1e6 for i in range(len(orig_psd))] if orig_psd else []
        deci_freqs_mhz = [f / 1e6 for f in deci_freqs_hz] if deci_freqs_hz and len(deci_freqs_hz) == len(deci_psd) else [i * deci_rate / (2 * len(deci_psd)) / 1e6 for i in range(len(deci_psd))] if deci_psd else []
        bands = data.get('bands', {})
        carrier_peaks = data.get('carrier_peaks', {})
        luma_floor = data.get('noise_floor_luma', {})
        chroma_floor = data.get('noise_floor_chroma', {})
        chroma_floor_method = data.get('noise_floor_chroma_method', {})
        luma_snr_basic = data.get('luma_snr', {})
        sync_snr_basic = data.get('sync_snr', {})
        white_snr_basic = data.get('white_snr', {})
        chroma_snr_basic = data.get('chroma_snr', {})

        def add_band_overlays(ax):
            if 'luma_signal' in bands:
                b = bands['luma_signal']
                ax.axvspan(b[0] / 1e6, b[1] / 1e6, alpha=0.10, color='#1f9d55', label='Luma band')
            if 'chroma_signal' in bands:
                b = bands['chroma_signal']
                ax.axvspan(b[0] / 1e6, b[1] / 1e6, alpha=0.14, color='#7e57c2', label='Chroma band')
            for key, color, label in [
                ('luma_noise_low', '#9e9e9e', 'Luma guard'),
                ('luma_noise_high', '#9e9e9e', None),
                ('chroma_noise_low', '#bdbdbd', 'Chroma guard'),
                ('chroma_noise_high', '#bdbdbd', None),
            ]:
                if key in bands:
                    b = bands[key]
                    ax.axvspan(b[0] / 1e6, b[1] / 1e6, alpha=0.08, color=color, label=label)

        def _psd_y_at_freq(freq_mhz, freqs, psd):
            """Return the PSD value at the nearest frequency bin, or None if arrays empty."""
            if not freqs or not psd:
                return None
            idx = min(range(len(freqs)), key=lambda i: abs(freqs[i] - freq_mhz))
            return psd[idx] if idx < len(psd) else None

        def add_peak_markers(ax, exclude_keys=None):
            peak_specs = [
                ('luma_orig',  '#1565C0', 'o', 'Orig luma pk',  freqs_mhz,      orig_psd),
                ('luma_deci',  '#E64A19', 'o', 'Cmp luma pk',  deci_freqs_mhz, deci_psd),
                ('sync_orig',  '#1565C0', '^', 'Orig sync pk',  freqs_mhz,      orig_psd),
                ('sync_deci',  '#E64A19', '^', 'Cmp sync pk',  deci_freqs_mhz, deci_psd),
                ('white_orig', '#1565C0', 's', 'Orig white pk', freqs_mhz,      orig_psd),
                ('white_deci', '#E64A19', 's', 'Cmp white pk', deci_freqs_mhz, deci_psd),
                ('chroma_orig','#1565C0', 'x', 'Orig chroma pk',freqs_mhz,      orig_psd),
                ('chroma_deci','#E64A19', 'x', 'Cmp chroma pk',deci_freqs_mhz, deci_psd),
            ]
            if exclude_keys is None:
                exclude_keys = set()
            for key, color, marker, label, fq, pq in peak_specs:
                peak = carrier_peaks.get(key)
                if peak:
                    x = peak['freq_hz'] / 1e6
                    mag = peak['mag_db']
                    y = _psd_y_at_freq(x, fq, pq)
                    if y is None:
                        continue
                    lbl = '_nolegend_' if key in exclude_keys else label
                    ax.plot([x], [y], marker=marker, markersize=7, color=color, linestyle='None', label=lbl)
                    ax.annotate(
                        f"{x:.3f} MHz\n{mag:.2f} dB",
                        (x, y),
                        xytext=(6, 6),
                        textcoords='offset points',
                        fontsize=7,
                        color=color,
                        bbox=dict(boxstyle='round,pad=0.2', facecolor='white', alpha=0.75, edgecolor=color)
                    )

        def add_luma_reference_lines(ax):
            for key, color, label in [
                ('luma_sync_ref_hz', '#546E7A', 'Sync ref'),
                ('luma_white_ref_hz', '#263238', 'White ref'),
            ]:
                if key in bands:
                    ax.axvline(bands[key] / 1e6, color=color, linestyle=':', linewidth=1.0, alpha=0.8, label=label)

        def set_panel_ylim(ax, series, floor_values=None):
            visible = []
            x0, x1 = ax.get_xlim()
            for freqs, psd in series:
                if not freqs or not psd:
                    continue
                for x, y in zip(freqs, psd):
                    if x0 <= x <= x1:
                        visible.append(y)
            if not visible:
                visible = orig_psd + deci_psd
            if floor_values:
                for v in floor_values:
                    if v is not None and isinstance(v, (int, float)):
                        visible.append(v)
            if visible:
                y_min = min(visible)
                y_max = max(visible)
                margin = max(2.0, (y_max - y_min) * 0.08)
                ax.set_ylim(y_min - margin, y_max + margin)

        def add_floor_lines(ax, floor_data, orig_color='#1565C0', deci_color='#E64A19', label_prefix='Floor', show_db=True):
            """Draw horizontal dashed lines at measured noise floor levels."""
            orig_val = floor_data.get('orig')
            deci_val = floor_data.get('deci')
            if orig_val is not None and isinstance(orig_val, (int, float)):
                lbl = f'{label_prefix} orig {orig_val:.1f} dB' if show_db else f'{label_prefix} orig'
                ax.axhline(y=orig_val, color=orig_color, linestyle=':', linewidth=1.0, alpha=0.7, label=lbl)
            if deci_val is not None and isinstance(deci_val, (int, float)):
                lbl = f'{label_prefix} cmp {deci_val:.1f} dB' if show_db else f'{label_prefix} cmp'
                ax.axhline(y=deci_val, color=deci_color, linestyle=':', linewidth=1.0, alpha=0.7, label=lbl)

        def add_metric_box(ax, lines, anchor=(0.01, 0.98)):
            text = "\n".join(lines)
            ax.text(
                anchor[0], anchor[1], text,
                transform=ax.transAxes,
                va='top', ha='left', fontsize=8,
                fontfamily='monospace',
                bbox=dict(boxstyle='round', facecolor='white', alpha=0.82, edgecolor='#666')
            )

        # Panel 1: Full spectrum
        ax1 = axes[0]
        if freqs_mhz and orig_psd:
            ax1.plot(freqs_mhz[:len(orig_psd)], orig_psd, linewidth=0.6, alpha=0.85, color='#1565C0', label='_nolegend_')
        if deci_freqs_mhz and deci_psd:
            ax1.plot(deci_freqs_mhz, deci_psd, linewidth=0.6, alpha=0.85, color='#E64A19', label='_nolegend_')
        add_band_overlays(ax1)
        add_peak_markers(ax1)
        add_luma_reference_lines(ax1)
        # Floor lines: angled from chroma floor to luma floor showing tilt
        chroma_guard_high = bands.get('chroma_noise_high', [950_000, 1_300_000])
        luma_guard_high = bands.get('luma_noise_high', [6_000_000, 7_000_000])
        # X positions: midpoint of chroma high guard, midpoint of luma high guard
        chroma_x = (chroma_guard_high[0] + chroma_guard_high[1]) / 2e6
        luma_x = (luma_guard_high[0] + luma_guard_high[1]) / 2e6
        chroma_orig = chroma_floor.get('orig')
        luma_orig = luma_floor.get('orig')
        chroma_deci = chroma_floor.get('deci')
        luma_deci = luma_floor.get('deci')
        if chroma_orig is not None and luma_orig is not None:
            ax1.plot([chroma_x, luma_x], [chroma_orig, luma_orig], color='#1565C0', linestyle=':', linewidth=1.2, alpha=0.7, label='Noise floor orig')
        if chroma_deci is not None and luma_deci is not None:
            ax1.plot([chroma_x, luma_x], [chroma_deci, luma_deci], color='#E64A19', linestyle=':', linewidth=1.2, alpha=0.7, label='Noise floor cmp')
        ax1.set_title('Full Spectrum')
        ax1.set_xlabel('Frequency (MHz)')
        ax1.set_ylabel('Magnitude (dB)')
        ax1.grid(True, alpha=0.25)
        ax1.set_xlim(0, max(max(freqs_mhz, default=0.0), max(deci_freqs_mhz, default=0.0), 1.0))
        set_panel_ylim(ax1, [(freqs_mhz, orig_psd), (deci_freqs_mhz, deci_psd)],
                       floor_values=[luma_floor.get('orig'), luma_floor.get('deci'),
                                     chroma_floor.get('orig'), chroma_floor.get('deci')])
        ax1.legend(loc='upper right', fontsize=8, ncol=2)

        # Panel 2: Luma zoom
        ax2 = axes[1]
        if freqs_mhz and orig_psd:
            ax2.plot(freqs_mhz[:len(orig_psd)], orig_psd, linewidth=0.8, alpha=0.9, color='#1565C0', label='_nolegend_')
        if deci_freqs_mhz and deci_psd:
            ax2.plot(deci_freqs_mhz, deci_psd, linewidth=0.8, alpha=0.9, color='#E64A19', label='_nolegend_')
        add_band_overlays(ax2)
        add_peak_markers(ax2)
        add_luma_reference_lines(ax2)
        add_floor_lines(ax2, luma_floor, label_prefix='Luma floor', show_db=False)
        luma_band = bands.get('luma_signal', [3_000_000, 5_500_000])
        ax2.set_xlim(max(0.0, luma_band[0] / 1e6 - 1.0), luma_band[1] / 1e6 + 1.0)
        ax2.set_title('Luma Spectrum')
        ax2.set_xlabel('Frequency (MHz)')
        ax2.set_ylabel('Magnitude (dB)')
        ax2.grid(True, alpha=0.25)
        set_panel_ylim(ax2, [(freqs_mhz, orig_psd), (deci_freqs_mhz, deci_psd)],
                       floor_values=[luma_floor.get('orig'), luma_floor.get('deci')])
        ax2.legend(loc='upper right', fontsize=8)
        add_metric_box(ax2, [
            f"Sync SNR orig : {sync_snr_basic.get('orig', float('nan')):7.2f} dB",
            f"Sync SNR cmp  : {sync_snr_basic.get('deci', float('nan')):7.2f} dB",
            f"White SNR orig: {white_snr_basic.get('orig', float('nan')):7.2f} dB",
            f"White SNR cmp : {white_snr_basic.get('deci', float('nan')):7.2f} dB",
        ])

        # Panel 3: Chroma zoom
        ax3 = axes[2]
        if freqs_mhz and orig_psd:
            ax3.plot(
                freqs_mhz[:len(orig_psd)],
                orig_psd,
                linewidth=1.0,
                alpha=0.95,
                color='#1565C0',
                linestyle='--',
                label='_nolegend_',
                zorder=2,
            )
        if deci_freqs_mhz and deci_psd:
            ax3.plot(
                deci_freqs_mhz,
                deci_psd,
                linewidth=1.2,
                alpha=1.0,
                color='#E64A19',
                linestyle='-',
                label='_nolegend_',
                zorder=4,
            )
        add_band_overlays(ax3)
        add_peak_markers(ax3, exclude_keys={'luma_orig', 'luma_deci', 'sync_orig', 'sync_deci', 'white_orig', 'white_deci'})
        add_floor_lines(ax3, chroma_floor, label_prefix='Chroma floor', show_db=False)
        chroma_band = bands.get('chroma_signal', [400_000, 900_000])
        ax3.set_xlim(max(0.0, chroma_band[0] / 1e6 - 0.25), chroma_band[1] / 1e6 + 0.25)
        ax3.set_title('Chroma Spectrum')
        ax3.set_xlabel('Frequency (MHz)')
        ax3.set_ylabel('Magnitude (dB)')
        ax3.grid(True, alpha=0.25)
        set_panel_ylim(ax3, [(freqs_mhz, orig_psd), (deci_freqs_mhz, deci_psd)],
                       floor_values=[chroma_floor.get('orig'), chroma_floor.get('deci')])
        ax3.legend(loc='upper right', fontsize=8)
        add_metric_box(ax3, [
            f"Chroma floor orig: {chroma_floor.get('orig', float('nan')):7.2f} dB ({chroma_floor_method.get('orig', '')})",
            f"Chroma floor cmp : {chroma_floor.get('deci', float('nan')):7.2f} dB ({chroma_floor_method.get('deci', '')})",
            f"Chroma SNR orig  : {chroma_snr_basic.get('orig', float('nan')):7.2f} dB",
            f"Chroma SNR cmp   : {chroma_snr_basic.get('deci', float('nan')):7.2f} dB",
            f"           Delta : {chroma_snr_basic.get('delta', float('nan')):+7.2f} dB",
        ])

        try:
            png_path = Path(self.original_file.get()).with_suffix('.compare_rf.png')
            fig.savefig(str(png_path), dpi=160, bbox_inches='tight')
            self.results_text.insert('end', f"\n\nSpectrum PNG saved: {png_path}")
        except Exception as save_error:
            self.results_text.insert('end', f"\n\nPNG save error: {save_error}")

        fig.show()

        # Separate Luma window
        fig_luma, ax_luma = plt.subplots(1, 1, figsize=(12, 6), constrained_layout=True)
        self._open_figures.append(fig_luma)
        fig_luma.canvas.mpl_connect('close_event', lambda _e: self._open_figures.remove(fig_luma) if fig_luma in self._open_figures else None)
        fig_luma.suptitle(f"Luma Spectrum - {data.get('standard', 'Unknown')}", fontsize=13)
        if freqs_mhz and orig_psd:
            ax_luma.plot(freqs_mhz[:len(orig_psd)], orig_psd, linewidth=0.8, alpha=0.9, color='#1565C0', label='_nolegend_')
        if deci_freqs_mhz and deci_psd:
            ax_luma.plot(deci_freqs_mhz, deci_psd, linewidth=0.8, alpha=0.9, color='#E64A19', label='_nolegend_')
        add_band_overlays(ax_luma)
        add_peak_markers(ax_luma)
        add_luma_reference_lines(ax_luma)
        add_floor_lines(ax_luma, luma_floor, label_prefix='Luma floor', show_db=False)
        luma_band = bands.get('luma_signal', [3_000_000, 5_500_000])
        ax_luma.set_xlim(max(0.0, luma_band[0] / 1e6 - 1.0), luma_band[1] / 1e6 + 1.0)
        ax_luma.set_xlabel('Frequency (MHz)')
        ax_luma.set_ylabel('Magnitude (dB)')
        ax_luma.grid(True, alpha=0.25)
        set_panel_ylim(ax_luma, [(freqs_mhz, orig_psd), (deci_freqs_mhz, deci_psd)],
                       floor_values=[luma_floor.get('orig'), luma_floor.get('deci')])
        ax_luma.legend(loc='upper right', fontsize=8)
        add_metric_box(ax_luma, [
            f"Sync SNR orig : {sync_snr_basic.get('orig', float('nan')):7.2f} dB",
            f"Sync SNR cmp  : {sync_snr_basic.get('deci', float('nan')):7.2f} dB",
            f"White SNR orig: {white_snr_basic.get('orig', float('nan')):7.2f} dB",
            f"White SNR cmp : {white_snr_basic.get('deci', float('nan')):7.2f} dB",
        ])
        fig_luma.show()

        # Separate Chroma window
        fig_chroma, ax_chroma = plt.subplots(1, 1, figsize=(12, 6), constrained_layout=True)
        self._open_figures.append(fig_chroma)
        fig_chroma.canvas.mpl_connect('close_event', lambda _e: self._open_figures.remove(fig_chroma) if fig_chroma in self._open_figures else None)
        fig_chroma.suptitle(f"Chroma Spectrum - {data.get('standard', 'Unknown')}", fontsize=13)
        if freqs_mhz and orig_psd:
            ax_chroma.plot(freqs_mhz[:len(orig_psd)], orig_psd, linewidth=1.0, alpha=0.95, color='#1565C0', linestyle='--', label='_nolegend_', zorder=2)
        if deci_freqs_mhz and deci_psd:
            ax_chroma.plot(deci_freqs_mhz, deci_psd, linewidth=1.2, alpha=1.0, color='#E64A19', linestyle='-', label='_nolegend_', zorder=4)
        add_band_overlays(ax_chroma)
        add_peak_markers(ax_chroma, exclude_keys={'luma_orig', 'luma_deci', 'sync_orig', 'sync_deci', 'white_orig', 'white_deci'})
        add_floor_lines(ax_chroma, chroma_floor, label_prefix='Chroma floor', show_db=False)
        chroma_band = bands.get('chroma_signal', [400_000, 900_000])
        ax_chroma.set_xlim(max(0.0, chroma_band[0] / 1e6 - 0.25), chroma_band[1] / 1e6 + 0.25)
        ax_chroma.set_xlabel('Frequency (MHz)')
        ax_chroma.set_ylabel('Magnitude (dB)')
        ax_chroma.grid(True, alpha=0.25)
        set_panel_ylim(ax_chroma, [(freqs_mhz, orig_psd), (deci_freqs_mhz, deci_psd)],
                       floor_values=[chroma_floor.get('orig'), chroma_floor.get('deci')])
        ax_chroma.legend(loc='upper right', fontsize=8)
        add_metric_box(ax_chroma, [
            f"Chroma floor orig: {chroma_floor.get('orig', float('nan')):7.2f} dB ({chroma_floor_method.get('orig', '')})",
            f"Chroma floor cmp : {chroma_floor.get('deci', float('nan')):7.2f} dB ({chroma_floor_method.get('deci', '')})",
            f"Chroma SNR orig  : {chroma_snr_basic.get('orig', float('nan')):7.2f} dB",
            f"Chroma SNR cmp   : {chroma_snr_basic.get('deci', float('nan')):7.2f} dB",
            f"           Delta : {chroma_snr_basic.get('delta', float('nan')):+7.2f} dB",
        ])
        fig_chroma.show()
    
    def _find_analyser(self) -> str:
        """Locate the signal_compare_analyser.py script."""
        script_dir = Path(__file__).parent
        candidates = [
            script_dir / "signal_compare_analyser.py",
            Path.cwd() / "signal_compare_analyser.py",
            Path(__file__).resolve().parent / "signal_compare_analyser.py",
        ]
        
        for candidate in candidates:
            if candidate.exists():
                return str(candidate)
        
        raise FileNotFoundError("signal_compare_analyser.py not found")

    def _find_backend_command(self):
        """Locate native Rust backend first, fallback to Python script."""
        # PyInstaller onefile extracts bundled data to sys._MEIPASS
        base_dir = Path(getattr(sys, '_MEIPASS', Path(__file__).parent))
        script_dir = Path(__file__).parent
        rust_candidates = [
            base_dir / "compare-rf.exe",
            script_dir / "compare-rf.exe",
            script_dir / "target" / "release" / "compare-rf.exe",
            Path.cwd() / "compare-rf.exe",
            Path.cwd() / "target" / "release" / "compare-rf.exe",
        ]
        for candidate in rust_candidates:
            if candidate.exists():
                return [str(candidate)]

        return ["python3", self._find_analyser()]

    def _initial_dir_for(self, selected_path: str) -> str:
        """Return the directory to open file dialog in."""
        if selected_path:
            p = Path(selected_path)
            if p.exists():
                return str(p.parent)
        return str(Path.home())


def main():
    root = Tk()
    gui = SignalCompareGUI(root)
    root.mainloop()


if __name__ == "__main__":
    main()
