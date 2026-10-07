//! Read-only compatibility probe for the optional native-desktop importer.
fn main() -> anyhow::Result<()> {
    leopardwm_platform_win32::set_dpi_awareness();
    let inventory = leopardwm_platform_win32::native_desktop_import::inventory()?;
    for (index, desktop) in inventory.desktops.iter().enumerate() {
        println!(
            "{}: {:?}: windows={}, manageable={}, pinned={}, current={}",
            index + 1,
            desktop.name,
            inventory
                .windows
                .iter()
                .filter(|w| w.desktop == desktop.id)
                .count(),
            inventory
                .windows
                .iter()
                .filter(|w| w.desktop == desktop.id && w.manageable)
                .count(),
            inventory
                .windows
                .iter()
                .filter(|w| w.desktop == desktop.id && w.pinned)
                .count(),
            desktop.id == inventory.current
        );
    }
    Ok(())
}
