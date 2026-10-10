"""Launch the learning window from a checkout, without installing the hardware bridge."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "bridge" / "src"))

from eve_bridge.learning.server import main

if __name__ == "__main__":
    raise SystemExit(main())
