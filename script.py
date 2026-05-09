import os
import pathlib
import shutil
import glob

while True:
    os.system("cargo clean -p pandora-toolchain")
    os.system("cargo build --timings")
    os.system("target\\debug\\pndc.exe")
