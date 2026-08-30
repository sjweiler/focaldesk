fn main() -> anyhow::Result<()> {
    // Historical directory name; this binary is the GTK right-edge system rail.
    focaldesk_shell_client::run(focaldesk_shell_client::ShellRole::Panel)
}
