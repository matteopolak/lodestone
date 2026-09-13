fn main() -> anyhow::Result<()> {
    let command = xtask::parse_cli_args(std::env::args().skip(1))?;
    xtask::run_cli_command(command)
}
