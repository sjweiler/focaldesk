#[derive(Debug, Default)]
pub struct Agent {
    pub name: String,
}

impl Agent {
    pub fn new(agent_name: String) -> Self {
        Self { name: agent_name }
    }
}
