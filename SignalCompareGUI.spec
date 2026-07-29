# -*- mode: python ; coding: utf-8 -*-
# PyInstaller spec for VHS RF Signal Analyser v6.0

import os

block_cipher = None

# Project paths
PROJECT_DIR = os.path.dirname(os.path.abspath(SPEC))

a = Analysis(
    [os.path.join(PROJECT_DIR, 'signal_compare_gui.py')],
    pathex=[],
    binaries=[],
    datas=[
        (os.path.join(PROJECT_DIR, 'signal_compare_analyser.py'), '.'),
        (os.path.join(PROJECT_DIR, 'target', 'release', 'compare-rf.exe'), '.'),
        (os.path.join(PROJECT_DIR, 'VERSION'), '.'),
    ],
    hiddenimports=[
        'matplotlib',
        'matplotlib.pyplot',
        'matplotlib.backends.backend_tkagg',
        'PIL._tkinter_finder',
    ],
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludedimports=[],
    win_no_prefer_redirects=False,
    win_private_assemblies=False,
    cipher=block_cipher,
    noarchive=False,
)

pyz = PYZ(a.pure, a.zipped_data, cipher=block_cipher)

exe = EXE(
    pyz,
    a.scripts,
    a.binaries,
    a.zipfiles,
    a.datas,
    [],
    name='VHSRFSignalAnalyser',
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=True,
    upx_exclude=[],
    runtime_tmpdir=None,
    console=False,
    disable_windowed_traceback=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
)
