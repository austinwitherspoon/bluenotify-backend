import sys
from pathlib import Path

package_parent_dir = Path(__file__).resolve().parent.parent

sys.path.append(str(package_parent_dir))
