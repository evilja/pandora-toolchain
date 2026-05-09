import os

while True:
    os.system("cargo build --timings")
    os.system("target\\debug\\pndc.exe")