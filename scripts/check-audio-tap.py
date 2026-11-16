#!/usr/bin/env python3
"""Exercise the native tap ring under sanitizers; never opens audio hardware."""
import argparse
import os
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--build-root', type=Path, default=Path('target/audio-tap-checks'))
args = parser.parse_args()
root = Path(__file__).resolve().parent.parent
out = args.build_root.resolve()
out.mkdir(parents=True, exist_ok=True)
sdk = os.environ.get('SDKROOT') or subprocess.check_output(
    ['xcrun', '--sdk', 'macosx', '--show-sdk-path'], text=True).strip()
binary = out / 'native-test'
subprocess.run(['xcrun', 'clang', '-isysroot', sdk, '-fobjc-arc',
                '-fsanitize=address,undefined', '-g', '-Wall', '-Wextra', '-Werror',
                '-framework', 'CoreAudio', '-framework', 'Foundation',
                str(root / 'src/audio_tap/native_test.m'), '-o', str(binary)], check=True)
subprocess.run([str(binary)], check=True, env={**os.environ, 'ASAN_OPTIONS': 'detect_leaks=0'})
