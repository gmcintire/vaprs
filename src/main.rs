use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "vaprs", version, about = "APRS iGate and digipeater")]
struct Cli {
    /// Configuration file path
    #[arg(short = 'f', long = "config", default_value = "/etc/vaprs/vaprs.toml")]
    config: String,

    /// Increase debug output (repeatable: -d, -dd, -ddd)
    #[arg(short = 'd', long = "debug", action = clap::ArgAction::Count)]
    debug: u8,

    /// Verbose output to stdout
    #[arg(short = 'v', long = "verbose")]
    verbose: bool,

    /// Output erlang data to syslog
    #[arg(short = 'e', long = "erlang")]
    erlang: bool,

    /// Keep in foreground (no daemonize)
    #[arg(short = 'i', long = "foreground")]
    foreground: bool,

    /// Log APRS-IS traffic
    #[arg(short = 'L', long = "log-aprsis")]
    log_aprsis: bool,
}

fn main() {
    let cli = Cli::parse();

    let _foreground = cli.foreground || cli.debug > 0 || cli.verbose || cli.erlang;

    if !std::path::Path::new(&cli.config).exists() {
        eprintln!("Configuration file not found: {}", cli.config);
        std::process::exit(1);
    }

    println!("vaprs starting with config: {}", cli.config);
}
