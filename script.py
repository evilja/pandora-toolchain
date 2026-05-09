import os
import pathlib
import shutil
import glob

while True:
    for path in glob.glob("target/debug/incremental/p*"):
        shutil.rmtree(path, ignore_errors=True)
    
    os.system("cargo build --timings")
    os.system("target\\debug\\pndc.exe")