fn main() -> anyhow::Result<()> {
    // Historical directory name; this binary is the GTK bottom task shelf.
    focaldesk_shell_client::run(focaldesk_shell_client::ShellRole::Dock)
}
