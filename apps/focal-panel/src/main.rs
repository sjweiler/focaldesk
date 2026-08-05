fn main() -> anyhow::Result<()> {
    focaldesk_shell_client::run(focaldesk_shell_client::ShellRole::Panel)
}
