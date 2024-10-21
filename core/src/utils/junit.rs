use serde::{Deserialize, Serialize};

use crate::types::errors::CoreError;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct JunitOutput {
    #[serde(rename = "time")]
    pub time: Option<f64>,
    #[serde(rename = "testsuite")]
    pub testsuites: Vec<TestSuite>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TestSuite {
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "time")]
    pub time: Option<f64>,
    #[serde(rename = "testcase")]
    pub testcases: Vec<TestCase>,
    #[serde(rename = "testsuite")]
    pub testsuites: Option<Vec<TestSuite>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TestCase {
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "classname")]
    pub classname: String,
    #[serde(rename = "time", default)]
    pub time: Option<f64>,
    pub failure: Option<Vec<Failure>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Failure {
    #[serde(rename = "message")]
    pub message: Option<String>,
    #[serde(rename = "type")]
    pub failure_type: Option<String>,
    #[serde(rename = "$value")]
    pub content: Option<String>,
}

impl JunitOutput {
    pub fn new_from_xml_str(xml_str: &str) -> Result<JunitOutput, CoreError> {
        let parsed: JunitOutput = serde_xml_rs::from_str(xml_str)?;
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_junit_xml() {
        const TEST_JUNIT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites time="15.682687">
    <testsuite name="Tests.Registration" time="6.605871">
        <testcase name="testCase1" classname="Tests.Registration" time="2.113871" />
        <testcase name="testCase2" classname="Tests.Registration" time="1.051" />
        <testcase name="testCase3" classname="Tests.Registration" time="3.441" />
    </testsuite>
    <testsuite name="Tests.Authentication" time="9.076816">
        <testsuite name="Tests.Authentication.Login" time="4.356">
            <testcase name="testCase4" classname="Tests.Authentication.Login" time="2.244" />
            <testcase name="testCase5" classname="Tests.Authentication.Login" time="0.781" />
            <testcase name="testCase6" classname="Tests.Authentication.Login" time="1.331" />
        </testsuite>
        <testcase name="testCase7" classname="Tests.Authentication" time="2.508" />
        <testcase name="testCase8" classname="Tests.Authentication" time="1.230816" />
        <testcase name="testCase9" classname="Tests.Authentication" time="0.982">
            <failure message="Assertion error message" type="AssertionError">
                <!-- Call stack printed here -->
                AAAA HELP WE FAILED
            </failure>
        </testcase>
    </testsuite>
</testsuites>"#;

        let parsed: JunitOutput = JunitOutput::new_from_xml_str(TEST_JUNIT_XML).unwrap();
        println!("{:?}", parsed);

        assert_eq!(parsed.testsuites.len(), 2);

        assert_eq!(parsed.testsuites[0].testcases.len(), 3);
        assert_eq!(parsed.testsuites[1].testcases.len(), 3);

        let first_testsuite = parsed.testsuites[0].clone();
        assert_eq!(first_testsuite.testcases.len(), 3);

        // test the failure message
        let second_testsuite = parsed.testsuites[1].clone();
        let failure = second_testsuite.testcases[2].failure.clone();
        assert!(failure.is_some() && failure.as_ref().unwrap().len() == 1);
        assert_eq!(
            failure.as_ref().unwrap()[0].content,
            Some("AAAA HELP WE FAILED".to_string())
        );
    }
}
