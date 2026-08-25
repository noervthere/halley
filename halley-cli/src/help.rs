pub const HELP: &str = "\
Usage: halleyctl <command>

Commands:
  outputs        List connected monitors and their current mode/position
  reload         Reload the selected configuration immediately
  capture        Enter Halley's native screenshot capture modes
  dpms           Control tty output power state
  node           List, inspect, focus, move, collapse, restore, toggle, or close nodes
  cluster        List, inspect, switch, or change cluster workspaces
  bearings       Show, hide, toggle, or inspect Bearings
  trail          Navigate or inspect per-monitor focus history
  monitor        Focus a directional or exactly named monitor
  stack          Cycle an active stacking cluster
  tile           Focus or swap cluster tiles
  portal         Inspect the desktop portal backend
  config         Edit or verify the configuration selected by the compositor
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

pub const CAPTURE_HELP: &str = "\
Usage:
  halleyctl capture menu [-o OUTPUT]
  halleyctl capture region [-o OUTPUT]
  halleyctl capture screen [-o OUTPUT]
  halleyctl capture window [-o OUTPUT]

The command waits until the capture is saved or cancelled.
";

pub const CONFIG_HELP: &str = "\
Usage:
  halleyctl config edit
  halleyctl config edit -c PATH
  halleyctl config edit --config PATH
  halleyctl config verify
  halleyctl config verify -c PATH
  halleyctl config verify --config PATH

`edit` uses $VISUAL, then $EDITOR, and falls back to vi.
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

pub const TRAIL_HELP: &str = "\
Usage:
  halleyctl trail prev [-o OUTPUT]
  halleyctl trail next [-o OUTPUT]
  halleyctl trail list [-o OUTPUT] [--json]
  halleyctl trail goto INDEX|SELECTOR [-o OUTPUT]

Selectors:
  focused, latest, ID, id:ID, title:TEXT, app:APP_ID
";

pub const MONITOR_HELP: &str = "\
Usage:
  halleyctl monitor focus left|right|up|down|OUTPUT
";

pub const STACK_HELP: &str = "\
Usage:
  halleyctl stack cycle forward [-o OUTPUT]
  halleyctl stack cycle backward [-o OUTPUT]
";

pub const TILE_HELP: &str = "\
Usage:
  halleyctl tile focus left|right|up|down [-o OUTPUT]
  halleyctl tile swap left|right|up|down [-o OUTPUT]
";

pub const PORTAL_HELP: &str = "\
Usage:
  halleyctl portal status [--json]
  halleyctl portal version [--json]
";

pub const DPMS_HELP: &str = "\
Usage:
  halleyctl dpms off|on|toggle [-o OUTPUT]
";
