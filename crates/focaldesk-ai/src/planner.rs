use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::agent::AgentToolSpec;

pub const MAX_AGENT_STEPS: usize = 4;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Plan {
    #[serde(default)]
    pub steps: Vec<PlanStep>,
    #[serde(default)]
    pub answer: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlanStep {
    pub tool: String,
    #[serde(default)]
    pub arguments: serde_json::Value,
}

#[derive(Debug, Default)]
pub struct Planner;

impl Planner {
    pub fn new() -> Self {
        Self
    }

    pub fn system_prompt(tools: &[AgentToolSpec]) -> Result<String> {
        let catalog = serde_json::to_string(tools).context("serialize agent tool catalog")?;
        Ok(format!(
            "You are the FocalDesk read-only desktop planner. Return exactly one JSON object and no markdown. \
             The schema is {{\"steps\":[{{\"tool\":\"name\",\"arguments\":{{}}}}],\"answer\":null}}. \
             Use no more than {MAX_AGENT_STEPS} steps. Only use tools in this catalog: {catalog}. \
             If no tool is needed, return an empty steps array and put the complete answer in `answer`. \
             Mutating tools may only be proposed, never considered executed. Never set or invent a `confirmed` argument. \
             Never invent tool names or arguments."
        ))
    }

    pub fn parse(content: &str, tools: &[AgentToolSpec]) -> Result<Plan> {
        let plan: Plan = serde_json::from_str(content.trim())
            .context("planner response must be a single JSON object")?;
        if plan.steps.len() > MAX_AGENT_STEPS {
            bail!("planner requested more than {MAX_AGENT_STEPS} tool steps");
        }
        for step in &plan.steps {
            let Some(_) = tools.iter().find(|tool| tool.name == step.tool) else {
                bail!("planner requested unknown tool: {}", step.tool);
            };
            if !step.arguments.is_object() {
                bail!("planner arguments for {} must be an object", step.tool);
            }
            if step.arguments.get("confirmed").is_some() {
                bail!("planner must not supply confirmation for {}", step.tool);
            }
        }
        let mutations = plan
            .steps
            .iter()
            .enumerate()
            .filter(|(_, step)| {
                tools
                    .iter()
                    .find(|tool| tool.name == step.tool)
                    .is_some_and(|tool| tool.mutating)
            })
            .collect::<Vec<_>>();
        if mutations.len() > 1 {
            bail!("planner may propose at most one mutating tool");
        }
        if mutations
            .first()
            .is_some_and(|(index, _)| *index + 1 != plan.steps.len())
        {
            bail!("a mutating tool must be the final plan step");
        }
        if plan.steps.is_empty() && plan.answer.as_deref().is_none_or(str::is_empty) {
            bail!("planner returned neither tool steps nor an answer");
        }
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tools() -> Vec<AgentToolSpec> {
        vec![AgentToolSpec {
            name: "list_windows".into(),
            description: "List windows".into(),
            input_schema: json!({"type": "object"}),
            mutating: false,
        }]
    }

    #[test]
    fn parser_accepts_allowlisted_read_only_steps() {
        let plan = Planner::parse(
            r#"{"steps":[{"tool":"list_windows","arguments":{}}],"answer":null}"#,
            &tools(),
        )
        .unwrap();
        assert_eq!(plan.steps[0].tool, "list_windows");
    }

    #[test]
    fn parser_rejects_unknown_tools_and_markdown_wrappers() {
        assert!(
            Planner::parse(r#"{"steps":[{"tool":"shell","arguments":{}}]}"#, &tools()).is_err()
        );
        assert!(Planner::parse("```json\n{\"steps\":[]}\n```", &tools()).is_err());
    }

    #[test]
    fn parser_rejects_model_supplied_confirmation() {
        let mut tools = tools();
        tools.push(AgentToolSpec {
            name: "focus_window".into(),
            description: "Focus a window".into(),
            input_schema: json!({"type": "object"}),
            mutating: true,
        });
        assert!(
            Planner::parse(
                r#"{"steps":[{"tool":"focus_window","arguments":{"window_id":7,"confirmed":true}}]}"#,
                &tools,
            )
            .is_err()
        );
    }
}
