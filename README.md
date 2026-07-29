# VHS-RF-Signal-Analyser
Compares two VHS RF capture files and measures signal. 
- Reports carrier frequencies
- noise floors
- signal-to-noise ratio for luma and chroma.
- Works with raw or FLAC ADC captures at any sample rate.
- Supports NTSC, PAL, M-PAL, N-PAL. Rust backend with Python/Tkinter GUI.

## Why use this and what can this tool help determine?
- Is your RF capture achieving the best possible signal-to-noise ratio?
- How does your capture setup compare against another setup?
- Is your capture device or VCR performing correctly?
- Are your capture settings, hardware modifications, and RF gain optimised?
- Is an analogue low-pass filter improving or degrading the capture quality?
- Is the RF head tap providing the best possible signal?
- Is an RF amplifier required, and is it configured correctly?
- What is the condition of the VCR video heads?
- Does decimation improve the captured RF signal-to-noise ratio?
- Does downsampling or resampling reduce recoverable signal quality?


## How To Build - Currently Windows
### 1. Install Rust (nightly, GNU toolchain)
winget install Rustlang.Rustup
rustup default nightly-x86_64-pc-windows-gnu

### 2. Install Python 3.14+ (if not present)
winget install Python.Python.3.14

### 3. Install Python packages
pip install matplotlib mutagen pyinstaller

### 4. Build
Set-Location C:\VHS_RF_Signal_Analyser
cargo build --release --bin compare-rf
python -m PyInstaller SignalCompareGUI.spec --distpath dist_v7-nn --noconfirm
Set-Location C:VHS_RF_Signal_Analyser
cargo build --release --bin compare-rf
python -m PyInstaller SignalCompareGUI.spec --distpath dist_v7-nn --noconfirm

**Output artifacts:**
CLI exe: target\release\compare-rf.exe
GUI exe: dist_v6\SignalCompareGUI.exe

## Usage:
Launch the gui tool: dist_v6.xx\SignalCompareGUI.exe
Or commandline tool: target\release\compare-rf.exe
