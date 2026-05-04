//! End-to-end tests for the `pixforge setup` wizard, using fake
//! implementations of the trait seams so no real TTY / network / fs
//! is touched.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{anyhow, Result};

use pixforge::config::Profile;
use pixforge::setup::{
    self, AppendOutcome, ConfigStore, ConnectionTester, EnvProbe, Prompter, ShellRcWriter,
    TestOutcome, WizardDeps,
};

// -- Scripted prompter -------------------------------------------------------

#[derive(Debug, Clone)]
enum Answer {
    Text(String),
    Choice(usize),
    Confirm(bool),
    Secret(String),
}

struct ScriptedPrompter {
    answers: VecDeque<Answer>,
    log: Vec<String>,
}

impl ScriptedPrompter {
    fn new(answers: Vec<Answer>) -> Self {
        Self {
            answers: answers.into(),
            log: Vec::new(),
        }
    }
}

impl Prompter for ScriptedPrompter {
    fn ask_text(&mut self, label: &str, _default: Option<&str>) -> Result<String> {
        self.log.push(format!("text: {label}"));
        match self.answers.pop_front() {
            Some(Answer::Text(s)) => Ok(s),
            other => Err(anyhow!("scripted text expected, got {other:?}")),
        }
    }
    fn ask_choice(&mut self, label: &str, _choices: &[String]) -> Result<usize> {
        self.log.push(format!("choice: {label}"));
        match self.answers.pop_front() {
            Some(Answer::Choice(n)) => Ok(n),
            other => Err(anyhow!("scripted choice expected, got {other:?}")),
        }
    }
    fn confirm(&mut self, label: &str, _default: bool) -> Result<bool> {
        self.log.push(format!("confirm: {label}"));
        match self.answers.pop_front() {
            Some(Answer::Confirm(b)) => Ok(b),
            other => Err(anyhow!("scripted confirm expected, got {other:?}")),
        }
    }
    fn ask_secret(&mut self, label: &str) -> Result<String> {
        self.log.push(format!("secret: {label}"));
        match self.answers.pop_front() {
            Some(Answer::Secret(s)) => Ok(s),
            other => Err(anyhow!("scripted secret expected, got {other:?}")),
        }
    }
    fn info(&mut self, msg: &str) {
        self.log.push(format!("info: {msg}"));
    }
    fn note(&mut self, msg: &str) {
        self.log.push(format!("note: {msg}"));
    }
}

// -- Fake config store -------------------------------------------------------

struct FakeConfig {
    contents: Mutex<Option<String>>,
}

impl FakeConfig {
    fn new(initial: Option<&str>) -> Self {
        Self {
            contents: Mutex::new(initial.map(String::from)),
        }
    }
    fn snapshot(&self) -> Option<String> {
        self.contents.lock().unwrap().clone()
    }
}

impl ConfigStore for FakeConfig {
    fn read(&self) -> Result<Option<String>> {
        Ok(self.contents.lock().unwrap().clone())
    }
    fn write(&self, contents: &str) -> Result<()> {
        *self.contents.lock().unwrap() = Some(contents.to_string());
        Ok(())
    }
    fn path(&self) -> PathBuf {
        PathBuf::from("/fake/pixforge/config.toml")
    }
}

// -- Fake shell rc writer ----------------------------------------------------

struct FakeRc {
    appended: RefCell<Vec<(String, String)>>,
    rc: Option<PathBuf>,
}

impl FakeRc {
    fn new(rc: Option<PathBuf>) -> Self {
        Self {
            appended: RefCell::new(Vec::new()),
            rc,
        }
    }
}

impl ShellRcWriter for FakeRc {
    fn rc_path(&self) -> Option<PathBuf> {
        self.rc.clone()
    }
    fn append_export(&self, var: &str, val: &str) -> Result<AppendOutcome> {
        let path = self.rc.clone().ok_or_else(|| anyhow!("no rc path"))?;
        // Idempotent: scan our log for an existing entry with same var.
        let mut log = self.appended.borrow_mut();
        if log.iter().any(|(v, _)| v == var) {
            return Ok(AppendOutcome::AlreadyPresent { path });
        }
        log.push((var.to_string(), val.to_string()));
        Ok(AppendOutcome::Appended { path })
    }
}

// -- Fake connection tester --------------------------------------------------

struct FakeTester {
    outcome: Mutex<Result<TestOutcome>>,
}
impl FakeTester {
    fn ok() -> Self {
        Self {
            outcome: Mutex::new(Ok(TestOutcome {
                bytes: 1234,
                latency_secs: 0.1,
                attempts: 1,
            })),
        }
    }
}
impl ConnectionTester for FakeTester {
    fn test(&mut self, _profile: &Profile) -> Result<TestOutcome> {
        std::mem::replace(
            &mut *self.outcome.lock().unwrap(),
            Err(anyhow!("test consumed")),
        )
    }
}

// -- Helpers ----------------------------------------------------------------

fn empty_env() -> EnvProbe<'static> {
    EnvProbe { get: &|_| None }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn happy_path_openai_compat_writes_profile_and_sets_default() {
    // OpenAI provider (idx 2 in PROVIDERS), defaults: endpoint api.openai.com,
    // model gpt-image-1, env var OPENAI_API_KEY, auth bearer.
    // Then: profile name = default ("openai-compat"), no test, no shell rc
    // edit (shell var present in env -> wizard skips), config has no default
    // before -> silently set as default.
    let answers = vec![
        Answer::Choice(2),                   // provider menu: openai-compat
        Answer::Text("OPENAI_API_KEY".into()), // api_key_env
        Answer::Text("https://api.openai.com/v1".into()), // endpoint
        Answer::Text("gpt-image-1".into()),  // model
        Answer::Choice(0),                   // auth_style: bearer
        Answer::Text("openai-compat".into()), // profile name (use default)
        Answer::Confirm(false),              // skip connection test
    ];
    let mut prompter = ScriptedPrompter::new(answers);
    let store = FakeConfig::new(None);
    let rc = FakeRc::new(Some(PathBuf::from("/fake/.zshrc")));
    let mut tester = FakeTester::ok();
    let env = EnvProbe {
        get: &|k| (k == "OPENAI_API_KEY").then(|| "fake-key".to_string()),
    };
    let mut deps = WizardDeps {
        prompter: &mut prompter,
        config: &store,
        shell_rc: &rc,
        tester: &mut tester,
        env,
    };
    let res = setup::run(&mut deps).expect("wizard should succeed");

    assert_eq!(res.profile_name, "openai-compat");
    assert!(res.set_as_default);
    let written = store.snapshot().expect("config was written");
    assert!(written.contains("default_profile = \"openai-compat\""));
    assert!(written.contains("[profile.openai-compat]"));
    assert!(written.contains("provider = \"openai-compat\""));
    assert!(written.contains("api_key_env = \"OPENAI_API_KEY\""));
    assert!(written.contains("auth_style = \"bearer\""));
    // Shell rc was NOT touched because env var is detected as set.
    assert!(rc.appended.borrow().is_empty());
}

#[test]
fn collision_detected_then_pick_new_name_path() {
    let existing = r#"
default_profile = "azure-mai"

[profile.azure-mai]
provider = "azure-mai"
endpoint = "https://x.services.ai.azure.com"
model = "MAI-Image-2"
api_key_env = "AZURE_API_KEY"
api_version = "preview"
"#;
    let answers = vec![
        Answer::Choice(0),                   // provider: azure-mai
        Answer::Text("AZURE_API_KEY".into()), // env var
        Answer::Text("https://y.services.ai.azure.com".into()),
        Answer::Text("MAI-Image-2e".into()),
        Answer::Text("preview".into()),
        Answer::Text("azure-mai".into()),    // collides with existing
        Answer::Choice(1),                   // pick a different name
        Answer::Text("azure-mai-fast".into()), // new name
        Answer::Confirm(false),              // skip test
        Answer::Confirm(false),              // don't change default_profile
    ];
    let mut prompter = ScriptedPrompter::new(answers);
    let store = FakeConfig::new(Some(existing));
    let rc = FakeRc::new(None);
    let mut tester = FakeTester::ok();
    let mut deps = WizardDeps {
        prompter: &mut prompter,
        config: &store,
        shell_rc: &rc,
        tester: &mut tester,
        env: empty_env(),
    };
    let res = setup::run(&mut deps).expect("wizard should succeed");

    assert_eq!(res.profile_name, "azure-mai-fast");
    assert!(!res.set_as_default);
    let written = store.snapshot().expect("written");
    assert!(written.contains("default_profile = \"azure-mai\"")); // unchanged
    assert!(written.contains("[profile.azure-mai]")); // unchanged
    assert!(written.contains("[profile.azure-mai-fast]")); // appended
    // Original endpoint preserved (toml_edit kept comments-and-formatting).
    assert!(written.contains("https://x.services.ai.azure.com"));
    assert!(written.contains("https://y.services.ai.azure.com"));
}

#[test]
fn invalid_existing_toml_is_refused() {
    let bad = "this is not [valid toml = yes\n";
    let answers = vec![
        Answer::Choice(0),
        Answer::Text("AZURE_API_KEY".into()),
        Answer::Text("https://x.services.ai.azure.com".into()),
        Answer::Text("MAI-Image-2".into()),
        Answer::Text("preview".into()),
        Answer::Text("azure-mai".into()),
    ];
    let mut prompter = ScriptedPrompter::new(answers);
    let store = FakeConfig::new(Some(bad));
    let rc = FakeRc::new(None);
    let mut tester = FakeTester::ok();
    let mut deps = WizardDeps {
        prompter: &mut prompter,
        config: &store,
        shell_rc: &rc,
        tester: &mut tester,
        env: empty_env(),
    };
    let err = setup::run(&mut deps).expect_err("must refuse to mutate broken config");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("safely edit") || msg.contains("syntax"),
        "got: {msg}"
    );
    // Config NOT written.
    assert_eq!(store.snapshot().as_deref(), Some(bad));
}

#[test]
fn shell_rc_offered_when_env_var_unset_and_user_accepts() {
    let answers = vec![
        Answer::Choice(2),                   // openai-compat
        Answer::Text("OPENAI_API_KEY".into()),
        Answer::Text("https://api.openai.com/v1".into()),
        Answer::Text("gpt-image-1".into()),
        Answer::Choice(0),                   // auth bearer
        Answer::Text("openai".into()),       // profile name
        Answer::Confirm(false),              // skip connection test
        Answer::Confirm(true),               // YES save secret to shell
        Answer::Secret("sk-test-1234".into()),
    ];
    let mut prompter = ScriptedPrompter::new(answers);
    let store = FakeConfig::new(None);
    let rc = FakeRc::new(Some(PathBuf::from("/fake/.zshrc")));
    let mut tester = FakeTester::ok();
    let mut deps = WizardDeps {
        prompter: &mut prompter,
        config: &store,
        shell_rc: &rc,
        tester: &mut tester,
        env: empty_env(), // OPENAI_API_KEY unset
    };
    let res = setup::run(&mut deps).expect("wizard should succeed");

    assert_eq!(res.profile_name, "openai");
    let appends = rc.appended.borrow();
    assert_eq!(appends.len(), 1);
    assert_eq!(appends[0].0, "OPENAI_API_KEY");
    assert_eq!(appends[0].1, "sk-test-1234");
}

#[test]
fn connection_test_failure_does_not_block_save() {
    let answers = vec![
        Answer::Choice(2),                   // openai-compat
        Answer::Text("OPENAI_API_KEY".into()),
        Answer::Text("https://api.openai.com/v1".into()),
        Answer::Text("gpt-image-1".into()),
        Answer::Choice(0),                   // auth bearer
        Answer::Text("openai".into()),
        Answer::Confirm(true),               // YES test connection
        // Test fails -> menu appears -> pick "Save profile anyway (skip test)"
        Answer::Choice(2),
    ];
    let mut prompter = ScriptedPrompter::new(answers);
    let store = FakeConfig::new(None);
    let rc = FakeRc::new(Some(PathBuf::from("/fake/.zshrc")));
    // Tester returns an error.
    struct ErrTester;
    impl ConnectionTester for ErrTester {
        fn test(&mut self, _: &Profile) -> Result<TestOutcome> {
            Err(anyhow!("simulated 401 unauthorized"))
        }
    }
    let mut tester = ErrTester;
    let env = EnvProbe {
        get: &|k| (k == "OPENAI_API_KEY").then(|| "fake-key".to_string()),
    };
    let mut deps = WizardDeps {
        prompter: &mut prompter,
        config: &store,
        shell_rc: &rc,
        tester: &mut tester,
        env,
    };
    let res = setup::run(&mut deps).expect("wizard saves even when test fails");
    assert_eq!(res.profile_name, "openai");
    assert!(res.test_outcome.unwrap().is_err());
    let written = store.snapshot().expect("written");
    assert!(written.contains("[profile.openai]"));
}
