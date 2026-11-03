//! A test run's result the engine's generative UI chose for a reply (a `ui`
//! part with `component: "tests"`): totals read from the run's output, and
//! the failing tests' names.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestRun {
    /// The command that ran it (`cargo t`); may be empty.
    pub command: String,
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
    pub failures: Vec<String>,
}

impl TestRun {
    /// A tests spec; `None` for another component or one with no tests.
    pub fn from_spec(spec: &Value) -> Option<Self> {
        if spec["component"] != "tests" {
            return None;
        }
        let n = |key: &str| spec[key].as_u64().unwrap_or(0);
        let run = Self {
            command: spec["command"].as_str().unwrap_or_default().to_string(),
            passed: n("passed"),
            failed: n("failed"),
            skipped: n("skipped"),
            failures: spec["failures"]
                .as_array()
                .map(|names| {
                    names
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        };
        (run.total() > 0).then_some(run)
    }

    pub fn total(&self) -> u64 {
        self.passed + self.failed + self.skipped
    }

    /// "All 172 tests passed", "2 of 174 tests failed".
    pub fn headline(&self) -> String {
        let ran = self.passed + self.failed;
        let tests = |n: u64| if n == 1 { "test" } else { "tests" };
        match self.failed {
            0 if ran == 1 => "1 test passed".to_string(),
            0 => format!("All {} {} passed", ran, tests(ran)),
            failed => format!("{failed} of {ran} {} failed", tests(ran)),
        }
    }

    /// For the transcript's height memo.
    pub fn content_len(&self) -> usize {
        self.failures.iter().map(|f| f.len() + 1).sum::<usize>() + self.command.len() + 3
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn headline_says_what_failed() {
        let run = |passed, failed| {
            TestRun::from_spec(&json!({ "component": "tests", "passed": passed, "failed": failed }))
                .unwrap()
        };
        assert_eq!(run(172, 0).headline(), "All 172 tests passed");
        assert_eq!(run(1, 0).headline(), "1 test passed");
        assert_eq!(run(170, 2).headline(), "2 of 172 tests failed");
        assert!(TestRun::from_spec(&json!({ "component": "tests" })).is_none());
    }
}
