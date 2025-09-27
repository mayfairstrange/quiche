/* src/main.rs */
mod experiments;
mod quic;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    // Single baseline request.
    // quic::single_get("https://localhost:4433/", "/dl_sponza.glb")?;

    // Run experiment A (different urgencies).
    experiments::experiment_initial_priorities(
        "https://localhost:4433/",
        "/dl_sponza.glb",
    )?;
    // experiments::experiment_priority_update(
    //     "https://localhost:4433/",
    //     "/dl_sponza.glb",
    // )?;

    Ok(())
}
