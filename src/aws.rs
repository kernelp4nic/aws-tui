use serde_json::Value;
use std::process::{Command, Stdio};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Service {
    Instances,
    Buckets,
    Vpcs,
    Subnets,
    SecurityGroups,
}
impl Service {
    pub const ALL: [Self; 5] = [
        Self::Instances,
        Self::Buckets,
        Self::Vpcs,
        Self::Subnets,
        Self::SecurityGroups,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Instances => "EC2 Instances",
            Self::Buckets => "S3 Buckets",
            Self::Vpcs => "VPCs",
            Self::Subnets => "Subnets",
            Self::SecurityGroups => "Security Groups",
        }
    }
    fn operation(self) -> (&'static str, &'static str, &'static str, &'static str) {
        match self {
            Self::Instances => ("ec2", "describe-instances", "", "InstanceId"),
            Self::Buckets => ("s3api", "list-buckets", "Buckets", "Name"),
            Self::Vpcs => ("ec2", "describe-vpcs", "Vpcs", "VpcId"),
            Self::Subnets => ("ec2", "describe-subnets", "Subnets", "SubnetId"),
            Self::SecurityGroups => (
                "ec2",
                "describe-security-groups",
                "SecurityGroups",
                "GroupId",
            ),
        }
    }
}
#[derive(Clone, Debug)]
pub struct Resource {
    pub id: String,
    pub name: String,
    pub state: String,
    pub kind: String,
    pub address: String,
    pub raw: Value,
}
impl Resource {
    pub fn searchable(&self) -> String {
        format!(
            "{} {} {} {} {} {}",
            self.id, self.name, self.state, self.kind, self.address, self.raw
        )
        .to_lowercase()
    }
}
#[derive(Clone)]
pub struct Client {
    pub profile: String,
    pub region: Option<String>,
    pub read_only: bool,
}
impl Client {
    pub fn session_command(&self, target: &str) -> Result<Command, String> {
        if self.read_only {
            return Err(
                "SSM sessions are blocked in read-only mode. Enable actions from the command palette.".into(),
            );
        }
        if !target.starts_with("i-")
            || !target[2..].chars().all(|c| c.is_ascii_hexdigit())
            || target.len() <= 2
        {
            return Err("Invalid EC2 instance ID".into());
        }
        let mut command = Command::new("aws");
        command
            .args([
                "ssm",
                "start-session",
                "--target",
                target,
                "--profile",
                &self.profile,
                "--no-cli-pager",
                "--no-cli-auto-prompt",
            ])
            .env("AWS_PAGER", "")
            .env("AWS_CLI_AUTO_PROMPT", "off")
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        if let Some(region) = &self.region {
            command.args(["--region", region]);
        }
        Ok(command)
    }

    pub fn run(&self, service: &str, operation: &str, extra: &[&str]) -> Result<Value, String> {
        let allowed = matches!(
            (service, operation),
            ("sts", "get-caller-identity")
                | (
                    "ec2",
                    "describe-instances"
                        | "describe-vpcs"
                        | "describe-subnets"
                        | "describe-security-groups"
                        | "describe-regions"
                        | "get-console-output"
                )
                | ("s3api", "list-buckets")
        );
        let mutation = service == "ec2"
            && matches!(
                operation,
                "start-instances" | "stop-instances" | "reboot-instances"
            );
        if !allowed && (!mutation || self.read_only) {
            return Err(
                "Operation blocked by backend policy (read-only or unsupported operation)".into(),
            );
        }
        let mut cmd = Command::new("aws");
        cmd.args([
            service,
            operation,
            "--profile",
            &self.profile,
            "--output",
            "json",
            "--no-cli-pager",
            "--no-cli-auto-prompt",
            "--cli-connect-timeout",
            "10",
            "--cli-read-timeout",
            "30",
        ])
        .args(extra)
        .env("AWS_PAGER", "")
        .env("AWS_CLI_AUTO_PROMPT", "off")
        .stdin(Stdio::null());
        if let Some(region) = &self.region {
            cmd.args(["--region", region]);
        }
        let output = cmd
            .output()
            .map_err(|e| format!("Cannot execute AWS CLI: {e}"))?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
        serde_json::from_slice(&output.stdout).map_err(|e| format!("Invalid AWS CLI JSON: {e}"))
    }
    pub fn load(&self, service: Service) -> Result<Vec<Resource>, String> {
        let (namespace, operation, collection, id) = service.operation();
        let data = self.run(namespace, operation, &[])?;
        Ok(parse(service, &data, collection, id))
    }
}
fn string(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or("—").into()
}
fn parse(service: Service, data: &Value, collection: &str, id: &str) -> Vec<Resource> {
    let values: Vec<&Value> = if service == Service::Instances {
        data["Reservations"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|r| r["Instances"].as_array().into_iter().flatten())
            .collect()
    } else {
        data[collection].as_array().into_iter().flatten().collect()
    };
    let mut rows: Vec<_> = values
        .into_iter()
        .map(|v| {
            let name = v["Tags"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|t| t["Key"] == "Name")
                .and_then(|t| t["Value"].as_str())
                .or(v["GroupName"].as_str())
                .unwrap_or("—")
                .to_string();
            Resource {
                id: string(v, id),
                name,
                state: v["State"]["Name"]
                    .as_str()
                    .or(v["State"].as_str())
                    .unwrap_or("—")
                    .into(),
                kind: string(v, "InstanceType"),
                address: v["PublicIpAddress"]
                    .as_str()
                    .or(v["PrivateIpAddress"].as_str())
                    .or(v["CidrBlock"].as_str())
                    .unwrap_or("—")
                    .into(),
                raw: v.clone(),
            }
        })
        .collect();
    rows.sort_by(|a, b| (&a.name, &a.id).cmp(&(&b.name, &b.id)));
    rows
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_all_reservations_and_missing_tags() {
        let v = serde_json::json!({"Reservations":[{"Instances":[{"InstanceId":"i-1","State":{"Name":"running"},"Tags":[{"Key":"Name","Value":"web"}]}]},{"Instances":[{"InstanceId":"i-2"}]}]});
        let rows = parse(Service::Instances, &v, "", "InstanceId");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|r| r.name == "web" && r.state == "running"));
    }
    #[test]
    fn ssm_uses_selected_target_and_context_and_enforces_policy() {
        let mut client = Client {
            profile: "work".into(),
            region: Some("us-east-2".into()),
            read_only: true,
        };
        assert!(client.session_command("i-0123456789abcdef0").is_err());
        client.read_only = false;
        assert!(client.session_command("--invalid").is_err());
        let command = client.session_command("i-0123456789abcdef0").unwrap();
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        assert_eq!(
            args,
            [
                "ssm",
                "start-session",
                "--target",
                "i-0123456789abcdef0",
                "--profile",
                "work",
                "--no-cli-pager",
                "--no-cli-auto-prompt",
                "--region",
                "us-east-2"
            ]
        );
    }
    #[test]
    fn read_only_blocks_before_spawning_cli() {
        let c = Client {
            profile: "default".into(),
            region: None,
            read_only: true,
        };
        assert!(c
            .run("ec2", "stop-instances", &["--instance-ids", "i-test"])
            .unwrap_err()
            .contains("blocked"));
        assert!(c.run("ec2", "terminate-instances", &[]).is_err());
    }
}
