import os
import sys

from krab import find_krab_bin

if __name__ == "__main__":
    krab = find_krab_bin()
    os.execv(krab, [krab, *sys.argv[1:]])
