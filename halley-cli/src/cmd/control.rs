use halley_api::{Direction, MonitorTarget, StackCycleDirection};

use super::Action;

pub(super) fn parse_monitor(args: &[String]) -> Result<Action, String> {
    match args.first().map(String::as_str) {
        None | Some("-h" | "--help") => Ok(Action::MonitorHelp),
        Some("focus") => {
            let target = args
                .get(1)
                .ok_or_else(|| "monitor focus requires a direction or output name".to_string())?;
            if let Some(unexpected) = args.get(2) {
                return Err(format!("unexpected monitor focus argument {unexpected:?}"));
            }
            let target = match target.as_str() {
                "left" => MonitorTarget::Direction(Direction::Left),
                "right" => MonitorTarget::Direction(Direction::Right),
                "up" => MonitorTarget::Direction(Direction::Up),
                "down" => MonitorTarget::Direction(Direction::Down),
                output => MonitorTarget::Output(output.to_string()),
            };
            Ok(Action::MonitorFocus(target))
        }
        Some(other) => Err(format!("unknown monitor command {other:?}")),
    }
}

pub(super) fn parse_stack(args: &[String]) -> Result<Action, String> {
    match args.first().map(String::as_str) {
        None | Some("-h" | "--help") => Ok(Action::StackHelp),
        Some("cycle") => {
            let direction = match args.get(1).map(String::as_str) {
                Some("forward") => StackCycleDirection::Forward,
                Some("backward") => StackCycleDirection::Backward,
                Some(other) => return Err(format!("unknown stack cycle direction {other:?}")),
                None => return Ok(Action::StackHelp),
            };
            let output = super::parse_output_option(&args[2..], "stack cycle")?;
            Ok(Action::StackCycle { direction, output })
        }
        Some(other) => Err(format!("unknown stack command {other:?}")),
    }
}

pub(super) fn parse_tile(args: &[String]) -> Result<Action, String> {
    let Some(command) = args.first().map(String::as_str) else {
        return Ok(Action::TileHelp);
    };
    if matches!(command, "-h" | "--help") {
        return Ok(Action::TileHelp);
    }
    let swap = match command {
        "focus" => false,
        "swap" => true,
        other => return Err(format!("unknown tile command {other:?}")),
    };
    let direction = args
        .get(1)
        .ok_or_else(|| format!("tile {command} requires a direction"))?;
    let direction = parse_direction(direction)?;
    let output = super::parse_output_option(&args[2..], &format!("tile {command}"))?;
    Ok(Action::Tile {
        direction,
        output,
        swap,
    })
}

fn parse_direction(value: &str) -> Result<Direction, String> {
    match value {
        "left" => Ok(Direction::Left),
        "right" => Ok(Direction::Right),
        "up" => Ok(Direction::Up),
        "down" => Ok(Direction::Down),
        other => Err(format!("unknown direction {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn parses_monitor_stack_and_tile_controls() {
        assert_eq!(
            parse_monitor(&args(&["focus", "DP-2"])),
            Ok(Action::MonitorFocus(MonitorTarget::Output("DP-2".into())))
        );
        assert_eq!(
            parse_stack(&args(&["cycle", "backward", "-o", "DP-1"])),
            Ok(Action::StackCycle {
                direction: StackCycleDirection::Backward,
                output: Some("DP-1".into()),
            })
        );
        assert_eq!(
            parse_tile(&args(&["swap", "up"])),
            Ok(Action::Tile {
                direction: Direction::Up,
                output: None,
                swap: true,
            })
        );
    }
}
