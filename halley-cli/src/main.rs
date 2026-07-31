mod cmd;
mod config;
mod help;
mod print;
mod transport;

use std::process::ExitCode;

use cmd::Action;
use halley_ipc::Request;

fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match cmd::parse(&args) {
        Ok(Action::Outputs) => transport::query(Request::Outputs, print::outputs),
        Ok(Action::Dpms { command, output }) => {
            transport::query(Request::Dpms { command, output }, print::ack)
        }
        Ok(Action::DpmsHelp) => show(help::DPMS_HELP),
        Ok(Action::Node { request, output }) => {
            transport::query(Request::Node(request), |response| {
                print::node(response, output)
            })
        }
        Ok(Action::NodeHelp) => show(help::NODE_HELP),
        Ok(Action::Cluster { request, output }) => {
            transport::query(Request::Cluster(request), |response| {
                print::cluster(response, output)
            })
        }
        Ok(Action::ClusterHelp) => show(help::CLUSTER_HELP),
        Ok(Action::Bearings(request)) => {
            transport::query(Request::Bearings(request), print::bearings)
        }
        Ok(Action::BearingsHelp) => show(help::BEARINGS_HELP),
        Ok(Action::ConfigVerify(path)) => config::verify(path),
        Ok(Action::ConfigHelp) => show(help::CONFIG_HELP),
        Ok(Action::Quit) => transport::query(Request::Quit, print::ack),
        Ok(Action::Version) => transport::query(Request::Version, print::version),
        Ok(Action::Help) => show(help::HELP),
        Err(err) => {
            eprintln!("halleyctl: {err}\n\n{}", help::HELP);
            ExitCode::from(2)
        }
    }
}

fn show(text: &str) -> ExitCode {
    print!("{text}");
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    #[test]
    fn help_separates_commands_from_options() {
        let commands = crate::help::HELP
            .split_once("Commands:\n")
            .and_then(|(_, rest)| rest.split_once("\nOptions:"))
            .map(|(commands, _)| commands)
            .expect("help has Commands followed by Options");

        assert!(commands.contains("outputs"));
        assert!(commands.contains("dpms"));
        assert!(commands.contains("node"));
        assert!(commands.contains("cluster"));
        assert!(commands.contains("bearings"));
        assert!(commands.contains("config"));
        assert!(commands.contains("quit"));
        assert!(!commands.contains("--help"));
        assert!(!commands.contains("--version"));
    }
}
