pub enum AppTarget {
    Terminal,
    Browser,
    FileManager,
    Command { program: String, args: Vec<String> },
}

pub struct SpawnRequest {
    pub target: AppTarget,
    pub cwd: Option<std::path::PathBuf>,
    pub env: Vec<(String, String)>,
    pub workspace: Option<u32>,
    pub floating: bool,
}

pub trait Spawner {
    fn spawn(&mut self, req: SpawnRequest) -> anyhow::Result<()>;
}
