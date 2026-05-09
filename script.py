import os

while True:
    os.system("cargo build")
    os.system("target\\debug\\pndc.exe")