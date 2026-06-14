//! Orchestration thread helpers.
//!
//! Kept isolated so the experimental `o` workflow can be removed without
//! touching handoff/session basics.

use mindplayer_core::{Agent, Command};
use std::path::{Path, PathBuf};

pub const MAX_CHILDREN: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Provider,
    Skill,
    Instruction,
    Children,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    ClaudeCode,
    Codex,
    Kiro,
}

#[derive(Debug, Clone)]
pub struct Draft {
    pub step: Step,
    pub provider: Provider,
    pub skill: String,
    pub instruction: String,
    pub children: usize,
}

#[derive(Debug, Clone, Default)]
pub struct BroadcastDraft {
    pub instruction: String,
    pub cursor: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchItem {
    pub lane: usize,
    pub instruction: String,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            step: Step::Provider,
            provider: Provider::Codex,
            skill: String::new(),
            instruction: String::new(),
            children: 3,
        }
    }
}

impl Provider {
    const ALL: [Self; 3] = [Self::ClaudeCode, Self::Codex, Self::Kiro];

    pub fn label(self) -> &'static str {
        match self {
            Self::ClaudeCode => "cc",
            Self::Codex => "codex",
            Self::Kiro => "kiro",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::Kiro => "Kiro",
        }
    }

    pub fn agent(self) -> Agent {
        match self {
            Self::ClaudeCode => Agent::Claude,
            Self::Codex => Agent::Codex,
            Self::Kiro => Agent::Kiro,
        }
    }

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|p| *p == self)
            .expect("provider is listed")
    }

    fn model_effort(self, role: LaneRole) -> Option<(&'static str, &'static str)> {
        match (self, role) {
            (Self::ClaudeCode, LaneRole::Main) => Some(("opus", "max")),
            (Self::ClaudeCode, LaneRole::Child) => Some(("sonnet", "ultracode")),
            (Self::Codex, _) => None,
            (Self::Kiro, LaneRole::Main) => Some(("opus", "max")),
            (Self::Kiro, LaneRole::Child) => Some(("sonnet", "max")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaneRole {
    Main,
    Child,
}

impl Draft {
    pub fn active_input_mut(&mut self) -> Option<&mut String> {
        match self.step {
            Step::Provider => None,
            Step::Skill => Some(&mut self.skill),
            Step::Instruction => Some(&mut self.instruction),
            Step::Children => None,
        }
    }

    pub fn push_text(&mut self, text: &str) {
        if let Some(buf) = self.active_input_mut() {
            buf.push_str(text);
        }
    }

    pub fn advance(&mut self) -> bool {
        match self.step {
            Step::Provider => {
                self.step = Step::Skill;
                false
            }
            Step::Skill => {
                self.step = Step::Instruction;
                false
            }
            Step::Instruction => {
                self.step = Step::Children;
                false
            }
            Step::Children => true,
        }
    }

    pub fn adjust_children(&mut self, delta: isize) {
        self.children = (self.children as isize + delta).clamp(1, MAX_CHILDREN as isize) as usize;
    }

    pub fn adjust_provider(&mut self, delta: isize) {
        let len = Provider::ALL.len() as isize;
        let index = (self.provider.index() as isize + delta).rem_euclid(len) as usize;
        self.provider = Provider::ALL[index];
    }

    pub fn set_children_digit(&mut self, c: char) {
        if let Some(n) = c.to_digit(10) {
            self.children = (n as usize).clamp(1, MAX_CHILDREN);
        }
    }

    pub fn set_provider_key(&mut self, c: char) {
        self.provider = match c {
            '1' | 'c' | 'C' => Provider::ClaudeCode,
            '2' | 'x' | 'X' | 'o' | 'O' => Provider::Codex,
            '3' | 'k' | 'K' => Provider::Kiro,
            _ => self.provider,
        };
    }
}

impl BroadcastDraft {
    pub fn push_text(&mut self, text: &str) {
        self.clamp_cursor();
        self.instruction.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    pub fn push_char(&mut self, c: char) {
        self.clamp_cursor();
        self.instruction.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn backspace(&mut self) {
        self.clamp_cursor();
        let Some(prev) = previous_boundary(&self.instruction, self.cursor) else {
            return;
        };
        self.instruction.drain(prev..self.cursor);
        self.cursor = prev;
    }

    pub fn delete(&mut self) {
        self.clamp_cursor();
        let Some(next) = next_boundary(&self.instruction, self.cursor) else {
            return;
        };
        self.instruction.drain(self.cursor..next);
    }

    pub fn move_left(&mut self) {
        self.clamp_cursor();
        if let Some(prev) = previous_boundary(&self.instruction, self.cursor) {
            self.cursor = prev;
        }
    }

    pub fn move_right(&mut self) {
        self.clamp_cursor();
        if let Some(next) = next_boundary(&self.instruction, self.cursor) {
            self.cursor = next;
        }
    }

    pub fn move_home(&mut self) {
        self.clamp_cursor();
        self.cursor = line_start(&self.instruction, self.cursor);
    }

    pub fn move_end(&mut self) {
        self.clamp_cursor();
        self.cursor = line_end(&self.instruction, self.cursor);
    }

    pub fn move_up(&mut self) {
        self.move_vertical(-1);
    }

    pub fn move_down(&mut self) {
        self.move_vertical(1);
    }

    fn move_vertical(&mut self, delta: isize) {
        self.clamp_cursor();
        let current_start = line_start(&self.instruction, self.cursor);
        let current_end = line_end(&self.instruction, self.cursor);
        let current_col = self.instruction[current_start..self.cursor].chars().count();
        let target = if delta < 0 {
            if current_start == 0 {
                return;
            }
            let prev_end = current_start.saturating_sub(1);
            let prev_start = line_start(&self.instruction, prev_end);
            Some((prev_start, prev_end))
        } else {
            if current_end >= self.instruction.len() {
                return;
            }
            let next_start = current_end + 1;
            let next_end = line_end(&self.instruction, next_start);
            Some((next_start, next_end))
        };
        let Some((target_start, target_end)) = target else {
            return;
        };
        self.cursor =
            byte_index_at_char_col(&self.instruction, target_start, target_end, current_col);
    }

    fn clamp_cursor(&mut self) {
        if self.cursor > self.instruction.len() {
            self.cursor = self.instruction.len();
        }
        while self.cursor > 0 && !self.instruction.is_char_boundary(self.cursor) {
            self.cursor -= 1;
        }
    }
}

fn previous_boundary(text: &str, cursor: usize) -> Option<usize> {
    text[..cursor].char_indices().last().map(|(index, _)| index)
}

fn next_boundary(text: &str, cursor: usize) -> Option<usize> {
    text[cursor..]
        .char_indices()
        .nth(1)
        .map(|(index, _)| cursor + index)
        .or_else(|| (cursor < text.len()).then_some(text.len()))
}

fn line_start(text: &str, cursor: usize) -> usize {
    text[..cursor].rfind('\n').map_or(0, |index| index + 1)
}

fn line_end(text: &str, cursor: usize) -> usize {
    text[cursor..]
        .find('\n')
        .map_or(text.len(), |index| cursor + index)
}

fn byte_index_at_char_col(text: &str, start: usize, end: usize, col: usize) -> usize {
    text[start..end]
        .char_indices()
        .nth(col)
        .map_or(end, |(index, _)| start + index)
}

pub fn main_label(draft: &Draft) -> String {
    let skill = fallback_skill(&draft.skill);
    format!("(orch {}){skill}", draft.provider.label())
}

pub fn child_label(draft: &Draft, index: usize) -> String {
    let skill = fallback_skill(&draft.skill);
    format!("(orch {} child {index}){skill}", draft.provider.label())
}

pub fn main_prompt(draft: &Draft, cwd: &Path) -> Vec<u8> {
    let skill = fallback_skill(&draft.skill);
    let profile = session_profile(draft.provider, LaneRole::Main);
    format!(
        "\
MindPlayer orchestration main session.

Provider:
{provider}

Session profile:
{profile}

Skill / mode to use:
{skill}

Task instruction:
{instruction}

Working directory:
{cwd}

Child lanes:
{children}

You are the coordinator. Treat the child lanes in this MindPlayer thread as independent reviewers/workers. Wait for child lane findings when needed, synthesize disagreements, and give the user a concise decision or next instruction. When the user gives more direction here, the child lanes will receive this lane's context through MindPlayer thread sync when opened.
",
        provider = draft.provider.title(),
        profile = profile,
        skill = skill,
        instruction = fallback_instruction(&draft.instruction),
        cwd = cwd.display(),
        children = draft.children,
    )
    .with_submit()
}

pub fn child_prompt(draft: &Draft, cwd: &Path, index: usize) -> Vec<u8> {
    let skill = fallback_skill(&draft.skill);
    let profile = session_profile(draft.provider, LaneRole::Child);
    format!(
        "\
MindPlayer orchestration child lane #{index}.

Provider:
{provider}

Session profile:
{profile}

Skill / mode to use:
{skill}

Task instruction:
{instruction}

Working directory:
{cwd}

Your job is to form an independent view. Inspect the repo or task state as needed, identify risks or missing evidence, and report findings for the main coordinator session. Do not make irreversible changes unless the main session explicitly delegates that work to this lane. When returning to this lane later, incorporate peer-lane context injected by MindPlayer thread sync.
",
        index = index,
        provider = draft.provider.title(),
        profile = profile,
        skill = skill,
        instruction = fallback_instruction(&draft.instruction),
        cwd = cwd.display(),
    )
    .with_submit()
}

pub fn broadcast_prompt(instruction: &str, cycle: u64) -> Vec<u8> {
    format!(
        "\
MindPlayer orchestration cycle #{cycle} instruction.

{instruction}

Work independently in this child lane, then report concise findings, blockers, and recommendations for the main coordinator lane. If this instruction conflicts with earlier child-lane work, follow this latest cycle instruction.
",
        instruction = fallback_instruction(instruction),
    )
    .with_submit()
}

pub fn dispatch_request_prompt(instruction: &str, cycle: u64, roster: &str) -> Vec<u8> {
    let example_lane = first_roster_lane(roster)
        .map(|lane| lane.to_string())
        .unwrap_or_else(|| "<listed lane number>".to_string());
    format!(
        "\
MindPlayer orchestration dispatch planning cycle #{cycle}.

The user wants the orchestration coordinator to decide which child lanes should
receive the next work, instead of broadcasting the same prompt to every lane.

User dispatch topic:
{instruction}

Available child lanes:
{roster}

Use the user dispatch topic and the available child lane roster to decide which
lanes should receive work. Use as few lanes as needed. Leave lanes idle when
they would duplicate work, increase conflict risk, or waste context.

Return a short explanation, then include exactly one dispatch block in this
format:

MINDPLAYER_DISPATCH
lane #{example_lane}:
<instruction for lane #{example_lane}, or idle>
END_MINDPLAYER_DISPATCH

Rules:
- Only include lanes that should receive a new instruction, plus any lane you
  explicitly want to mark idle.
- Put each lane instruction under its own `lane #<listed lane number>:` heading.
- Use `idle`, `skip`, or `noop` for lanes that should not receive work.
- Do not invent lane numbers not listed in the roster.
- Do not implement in this main lane. Coordinate and dispatch only.
",
        instruction = fallback_instruction(instruction),
        roster = roster,
        example_lane = example_lane
    )
    .with_submit()
}

pub fn dispatch_child_prompt(instruction: &str, cycle: u64, lane: usize) -> Vec<u8> {
    format!(
        "\
MindPlayer orchestration dispatch cycle #{cycle} for child lane #{lane}.

{instruction}

This instruction was selected by the main coordinator for this lane. Work only
on this assigned slice, avoid overlapping unrelated lane work, and report files
changed, tests run, blockers, and completion status back to the main lane.
",
        instruction = fallback_instruction(instruction)
    )
    .with_submit()
}

pub fn peer_review_prompt(cycle: u64) -> Vec<u8> {
    format!(
        "\
MindPlayer orchestration peer-review cycle #{cycle}.

Review the peer lane context above before answering.

Your task:
1. Identify what you agree with in the peer lane findings.
2. Identify what you disagree with or would change.
3. Point out risks, missing evidence, or UX gaps that peers missed.
4. Give one final recommendation to the main coordinator.

Do not implement. Do not edit files. Do not run broad refactors. This round is for critique and recommendation only.
"
    )
    .with_submit()
}

pub fn parse_dispatch_plan(text: &str) -> Vec<DispatchItem> {
    let Some(block) = dispatch_block(text) else {
        return Vec::new();
    };
    let mut items = Vec::new();
    let mut current_lane = None;
    let mut current_body = String::new();
    for line in block.lines() {
        if let Some(lane) = parse_lane_heading(line) {
            push_dispatch_item(&mut items, current_lane, &current_body);
            current_lane = Some(lane);
            current_body.clear();
            if let Some((_, rest)) = line.split_once(':') {
                current_body.push_str(rest.trim_start());
            }
            continue;
        }
        if current_lane.is_some() {
            if !current_body.is_empty() {
                current_body.push('\n');
            }
            current_body.push_str(line);
        }
    }
    push_dispatch_item(&mut items, current_lane, &current_body);
    items
}

fn dispatch_block(text: &str) -> Option<&str> {
    let mut start = None;
    for (index, line) in text.match_indices("MINDPLAYER_DISPATCH") {
        if text[..index]
            .chars()
            .last()
            .is_none_or(|ch| ch == '\n' || ch == '\r')
            && line == "MINDPLAYER_DISPATCH"
        {
            start = Some(index + line.len());
        }
    }
    let start = start?;
    let rest = &text[start..];
    let end = rest.find("END_MINDPLAYER_DISPATCH")?;
    Some(&rest[..end])
}

fn parse_lane_heading(line: &str) -> Option<usize> {
    let trimmed = line.trim();
    let lower = trimmed.to_ascii_lowercase();
    let rest = lower.strip_prefix("lane #")?;
    let digits = rest
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    if digits.is_empty() || !rest[digits.len()..].trim_start().starts_with(':') {
        return None;
    }
    digits.parse().ok()
}

fn first_roster_lane(roster: &str) -> Option<usize> {
    roster.lines().find_map(|line| {
        let trimmed = line.trim_start();
        parse_lane_heading(trimmed.strip_prefix("- ").unwrap_or(trimmed))
    })
}

fn push_dispatch_item(items: &mut Vec<DispatchItem>, lane: Option<usize>, body: &str) {
    let Some(lane) = lane else {
        return;
    };
    let instruction = body.trim().to_string();
    if instruction.is_empty() || is_idle_instruction(&instruction) {
        return;
    }
    items.push(DispatchItem { lane, instruction });
}

fn is_idle_instruction(instruction: &str) -> bool {
    matches!(
        instruction.trim().to_ascii_lowercase().as_str(),
        "idle" | "skip" | "noop" | "none" | "no-op"
    )
}

pub fn synthesis_prompt(cycle: u64) -> Vec<u8> {
    format!(
        "\
MindPlayer orchestration synthesis cycle #{cycle}.

Review the latest child lane transcripts above before answering. Treat the tail
of each transcript as the most important source of truth, because child lanes may
have continued from design/review into implementation.

Your task:
1. Summarize what each child lane most recently did, including files changed,
   tests run, failures, blockers, and any explicit completion claims.
2. Identify which work appears complete, duplicated, conflicting, stale, or still
   missing.
3. Decide the current integrated state of the orchestration: done, needs review,
   needs fixes, or needs another child-lane cycle.
4. If fixes remain, produce the next concrete implementation instruction and
   name the lane(s) that should receive it.
5. If everything appears complete, produce the final user-facing summary and
   verification checklist.

Do not summarize only proposals or old peer reviews. Prefer the latest observed
implementation results from the child lanes. Do not implement in this response
unless the user explicitly asked the main coordinator to implement now.
"
    )
    .with_submit()
}

pub fn agent_for_main(draft: &Draft) -> Agent {
    draft.provider.agent()
}

pub fn agent_for_child(draft: &Draft, _index: usize) -> Agent {
    draft.provider.agent()
}

pub fn main_command(draft: &Draft, cwd: PathBuf) -> Command {
    session_command(draft.provider, LaneRole::Main, cwd)
}

pub fn child_command(draft: &Draft, cwd: PathBuf, _index: usize) -> Command {
    session_command(draft.provider, LaneRole::Child, cwd)
}

fn session_command(provider: Provider, role: LaneRole, cwd: PathBuf) -> Command {
    let mut command = mindplayer_core::new_session(provider.agent(), cwd);
    if let Some((model, effort)) = provider.model_effort(role) {
        command.args.extend([
            "--model".to_string(),
            model.to_string(),
            "--effort".to_string(),
            effort.to_string(),
        ]);
    }
    command
}

fn session_profile(provider: Provider, role: LaneRole) -> String {
    match provider.model_effort(role) {
        Some((model, effort)) => format!("{} --model {model} --effort {effort}", provider.label()),
        None => format!("{} default session", provider.label()),
    }
}

fn fallback_skill(skill: &str) -> String {
    let skill = skill.trim();
    if skill.is_empty() {
        "general orchestration".to_string()
    } else {
        skill.to_string()
    }
}

fn fallback_instruction(instruction: &str) -> String {
    let instruction = instruction.trim();
    if instruction.is_empty() {
        "Coordinate on the current user task.".to_string()
    } else {
        instruction.to_string()
    }
}

trait SubmitPrompt {
    fn with_submit(self) -> Vec<u8>;
}

impl SubmitPrompt for String {
    fn with_submit(mut self) -> Vec<u8> {
        self.push('\r');
        self.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn child_count_is_clamped() {
        let mut draft = Draft::default();
        draft.set_children_digit('9');
        assert_eq!(draft.children, MAX_CHILDREN);
        draft.adjust_children(-99);
        assert_eq!(draft.children, 1);
    }

    #[test]
    fn broadcast_draft_supports_cursor_editing() {
        let mut draft = BroadcastDraft::default();
        draft.push_text("abc\n한글");
        assert_eq!(draft.cursor, "abc\n한글".len());
        draft.move_left();
        draft.move_left();
        draft.push_char('!');
        assert_eq!(draft.instruction, "abc\n!한글");
        draft.move_home();
        draft.push_text("리뷰 ");
        assert_eq!(draft.instruction, "abc\n리뷰 !한글");
        draft.move_end();
        draft.backspace();
        assert_eq!(draft.instruction, "abc\n리뷰 !한");
        draft.move_up();
        draft.delete();
        assert_eq!(draft.instruction, "abc리뷰 !한");
    }

    #[test]
    fn prompts_submit_and_include_task_shape() {
        let draft = Draft {
            step: Step::Children,
            provider: Provider::Codex,
            skill: "$ralplan".into(),
            instruction: "compare alternatives".into(),
            children: 2,
        };
        let prompt = String::from_utf8(main_prompt(&draft, Path::new("/work"))).unwrap();
        assert!(prompt.contains("MindPlayer orchestration main session"));
        assert!(prompt.contains("$ralplan"));
        assert!(prompt.contains("compare alternatives"));
        assert!(prompt.ends_with('\r'));
    }

    #[test]
    fn provider_profiles_build_expected_commands() {
        let mut draft = Draft {
            step: Step::Children,
            provider: Provider::Codex,
            skill: "$ralplan".into(),
            instruction: "compare alternatives".into(),
            children: 2,
        };
        let codex = main_command(&draft, PathBuf::from("/work"));
        assert_eq!(codex.program, "codex");
        assert!(codex.args.is_empty());

        draft.provider = Provider::ClaudeCode;
        let cc_main = main_command(&draft, PathBuf::from("/work"));
        assert_eq!(cc_main.program, "claude");
        assert_eq!(cc_main.args, ["--model", "opus", "--effort", "max"]);
        let cc_child = child_command(&draft, PathBuf::from("/work"), 1);
        assert_eq!(cc_child.program, "claude");
        assert_eq!(
            cc_child.args,
            ["--model", "sonnet", "--effort", "ultracode"]
        );

        draft.provider = Provider::Kiro;
        let kiro_main = main_command(&draft, PathBuf::from("/work"));
        assert_eq!(kiro_main.program, "kiro-cli");
        assert_eq!(
            kiro_main.args,
            ["chat", "--model", "opus", "--effort", "max"]
        );
        let kiro_child = child_command(&draft, PathBuf::from("/work"), 1);
        assert_eq!(kiro_child.program, "kiro-cli");
        assert_eq!(
            kiro_child.args,
            ["chat", "--model", "sonnet", "--effort", "max"]
        );
    }

    #[test]
    fn broadcast_prompt_supports_multiline_instruction() {
        let prompt = String::from_utf8(broadcast_prompt("검토 A\nreview B", 2)).unwrap();
        assert!(prompt.contains("cycle #2"));
        assert!(prompt.contains("검토 A\nreview B"));
        assert!(prompt.ends_with('\r'));
    }

    #[test]
    fn dispatch_prompt_requests_strict_block() {
        let prompt = String::from_utf8(dispatch_request_prompt(
            "fix focused bug",
            3,
            "- lane #2: codex",
        ))
        .unwrap();
        assert!(prompt.contains("dispatch planning cycle #3"));
        assert!(prompt.contains("fix focused bug"));
        assert!(prompt.contains("MINDPLAYER_DISPATCH"));
        assert!(prompt.contains("END_MINDPLAYER_DISPATCH"));
        assert!(prompt.contains("lane #2:"));
        assert!(!prompt.contains("lane #N:"));
        assert!(!prompt.contains("lane #1:"));
        assert!(!prompt.contains("lane #3:"));
        assert!(prompt.ends_with('\r'));
    }

    #[test]
    fn parse_dispatch_plan_uses_latest_block_and_skips_idle_lanes() {
        let parsed = parse_dispatch_plan(
            "\
old
MINDPLAYER_DISPATCH
lane #1:
old work
END_MINDPLAYER_DISPATCH

new
MINDPLAYER_DISPATCH
lane #1:
idle
lane #2:
Implement fix.
lane #3: Review tests only.
END_MINDPLAYER_DISPATCH",
        );
        assert_eq!(
            parsed,
            vec![
                DispatchItem {
                    lane: 2,
                    instruction: "Implement fix.".into()
                },
                DispatchItem {
                    lane: 3,
                    instruction: "Review tests only.".into()
                }
            ]
        );
    }
}
