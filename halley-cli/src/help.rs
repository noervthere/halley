pub const HELP: &str = "\
Usage: halleyctl <command>

Commands:
  outputs        List connected monitors and their current mode/position
  dpms           Control tty output power state
  node           List, inspect, focus, move, collapse, restore, toggle, or close nodes
  cluster        List, inspect, switch, or change cluster workspaces
  bearings       Show, hide, toggle, or inspect Bearings
  config         Verify the configuration selected by the compositor
  quit           Open Halley's exit confirmation

Options:
  -h, --help     Print this message
  -V, --version  Print both halleyctl's and the running compositor's version
";

pub const CLUSTER_HELP: &str = "\
Usage:
  halleyctl cluster list [-o OUTPUT] [--json]
  halleyctl cluster info [current|ID|id:ID] [-o OUTPUT] [--json]
  halleyctl cluster layout-cycle [-o OUTPUT]
  halleyctl cluster slot 1..10 [-o OUTPUT]

Without -o, current and control commands use the selected monitor.
";

pub const CONFIG_HELP: &str = "\
Usage:
  halleyctl config verify
  halleyctl config verify -c PATH
  halleyctl config verify --config PATH
";

pub const NODE_HELP: &str = "\
Usage:
  halleyctl node list [-o OUTPUT] [--json]
  halleyctl node info [SELECTOR] [-o OUTPUT] [--json]
  halleyctl node focus [SELECTOR] [-o OUTPUT]
  halleyctl node move left|right|up|down [SELECTOR] [-o OUTPUT]
  halleyctl node collapse [SELECTOR] [-o OUTPUT]
  halleyctl node restore [SELECTOR] [-o OUTPUT]
  halleyctl node toggle [SELECTOR] [-o OUTPUT]
  halleyctl node close [SELECTOR] [-o OUTPUT]

Selectors:
  focused, latest, ID, id:ID, title:TEXT, app:APP_ID

Markers:
  * focused node
  + latest node
  - other node
";

pub const BEARINGS_HELP: &str = "\
Usage:
  halleyctl bearings show
  halleyctl bearings hide
  halleyctl bearings toggle
  halleyctl bearings status
";

pub const DPMS_HELP: &str = "\
Usage:
  halleyctl dpms off|on|toggle [-o OUTPUT]
";
