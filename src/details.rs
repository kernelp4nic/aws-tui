use crate::aws::{Resource, Service};
use serde_json::Value;

pub struct Field {
    pub label: String,
    pub value: String,
    pub section: bool,
}
fn section(rows: &mut Vec<Field>, title: &str) {
    rows.push(Field {
        label: title.into(),
        value: String::new(),
        section: true,
    });
}
fn field(rows: &mut Vec<Field>, label: &str, value: impl Into<String>) {
    let value = value.into();
    if !value.is_empty() && value != "—" {
        rows.push(Field {
            label: label.into(),
            value,
            section: false,
        });
    }
}
fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Bool(b) => if *b { "Yes" } else { "No" }.into(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}
fn properties(rows: &mut Vec<Field>, raw: &Value, keys: &[(&str, &str)]) {
    for (label, path) in keys {
        field(
            rows,
            label,
            scalar(raw.pointer(path).unwrap_or(&Value::Null)),
        );
    }
}
pub fn fields(service: Service, resource: &Resource) -> Vec<Field> {
    let mut rows = Vec::new();
    let raw = &resource.raw;
    section(&mut rows, "Overview");
    field(&mut rows, "Name", &resource.name);
    field(&mut rows, "ID", &resource.id);
    field(&mut rows, "State", &resource.state);
    match service {
        Service::Instances => {
            properties(
                &mut rows,
                raw,
                &[
                    ("Instance type", "/InstanceType"),
                    ("Availability zone", "/Placement/AvailabilityZone"),
                    ("Launched", "/LaunchTime"),
                    ("Architecture", "/Architecture"),
                    ("Platform", "/PlatformDetails"),
                    ("Image", "/ImageId"),
                    ("Key pair", "/KeyName"),
                    ("IAM profile", "/IamInstanceProfile/Arn"),
                    ("State reason", "/StateTransitionReason"),
                ],
            );
            section(&mut rows, "Network");
            properties(
                &mut rows,
                raw,
                &[
                    ("Private IP", "/PrivateIpAddress"),
                    ("Public IP", "/PublicIpAddress"),
                    ("Private DNS", "/PrivateDnsName"),
                    ("Public DNS", "/PublicDnsName"),
                    ("VPC", "/VpcId"),
                    ("Subnet", "/SubnetId"),
                ],
            );
            for group in raw["SecurityGroups"].as_array().into_iter().flatten() {
                field(
                    &mut rows,
                    "Security group",
                    format!(
                        "{} ({})",
                        scalar(&group["GroupName"]),
                        scalar(&group["GroupId"])
                    ),
                );
            }
            section(&mut rows, "Storage");
            properties(
                &mut rows,
                raw,
                &[
                    ("Root device", "/RootDeviceName"),
                    ("Root type", "/RootDeviceType"),
                ],
            );
            for device in raw["BlockDeviceMappings"].as_array().into_iter().flatten() {
                field(
                    &mut rows,
                    &scalar(&device["DeviceName"]),
                    format!(
                        "{} · {} · delete on termination: {}",
                        scalar(&device["Ebs"]["VolumeId"]),
                        scalar(&device["Ebs"]["Status"]),
                        scalar(&device["Ebs"]["DeleteOnTermination"])
                    ),
                );
            }
        }
        Service::Buckets => properties(
            &mut rows,
            raw,
            &[
                ("Created", "/CreationDate"),
                ("Region", "/BucketRegion"),
                ("ARN", "/BucketArn"),
            ],
        ),
        Service::Vpcs => properties(
            &mut rows,
            raw,
            &[
                ("CIDR", "/CidrBlock"),
                ("Default VPC", "/IsDefault"),
                ("Tenancy", "/InstanceTenancy"),
                ("Owner", "/OwnerId"),
            ],
        ),
        Service::Subnets => properties(
            &mut rows,
            raw,
            &[
                ("VPC", "/VpcId"),
                ("CIDR", "/CidrBlock"),
                ("Availability zone", "/AvailabilityZone"),
                ("Available IPs", "/AvailableIpAddressCount"),
                ("Auto public IP", "/MapPublicIpOnLaunch"),
                ("Owner", "/OwnerId"),
            ],
        ),
        Service::SecurityGroups => {
            properties(
                &mut rows,
                raw,
                &[
                    ("Description", "/Description"),
                    ("VPC", "/VpcId"),
                    ("Owner", "/OwnerId"),
                ],
            );
            for (title, key) in [
                ("Inbound rules", "IpPermissions"),
                ("Outbound rules", "IpPermissionsEgress"),
            ] {
                section(&mut rows, title);
                let rules = raw[key].as_array().map(Vec::as_slice).unwrap_or_default();
                if rules.is_empty() {
                    field(&mut rows, "Rules", "None");
                }
                for rule in rules {
                    let protocol = scalar(&rule["IpProtocol"]);
                    let protocol = if protocol == "-1" {
                        "All traffic"
                    } else {
                        &protocol
                    };
                    let from = scalar(&rule["FromPort"]);
                    let to = scalar(&rule["ToPort"]);
                    let ports = if from.is_empty() {
                        String::new()
                    } else if from == to {
                        format!(" {from}")
                    } else {
                        format!(" {from}–{to}")
                    };
                    let mut peers = Vec::new();
                    for (key, value) in [
                        ("IpRanges", "CidrIp"),
                        ("Ipv6Ranges", "CidrIpv6"),
                        ("UserIdGroupPairs", "GroupId"),
                        ("PrefixListIds", "PrefixListId"),
                    ] {
                        for peer in rule[key].as_array().into_iter().flatten() {
                            peers.push(scalar(&peer[value]));
                        }
                    }
                    field(&mut rows, &format!("{protocol}{ports}"), peers.join(", "));
                }
            }
        }
    }
    if let Some(tags) = raw["Tags"].as_array().filter(|tags| !tags.is_empty()) {
        section(&mut rows, "Tags");
        let mut tags: Vec<_> = tags.iter().collect();
        tags.sort_by_key(|tag| tag["Key"].as_str().unwrap_or_default());
        for tag in tags {
            field(&mut rows, &scalar(&tag["Key"]), scalar(&tag["Value"]));
        }
    }
    rows
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn instance_details_group_useful_fields_and_tags_without_json() {
        let resource = Resource {
            id: "i-test".into(),
            name: "Web 🇺🇾".into(),
            state: "running".into(),
            kind: "t3.micro".into(),
            address: "10.0.0.1".into(),
            raw: serde_json::json!({"InstanceType":"t3.micro", "PrivateIpAddress":"10.0.0.1", "Tags":[{"Key":"Team","Value":"Platform"}], "BlockDeviceMappings":[{"DeviceName":"/dev/xvda","Ebs":{"VolumeId":"vol-test","Status":"attached","DeleteOnTermination":true}}]}),
        };
        let rows = fields(Service::Instances, &resource);
        assert!(rows
            .iter()
            .any(|r| r.label == "Private IP" && r.value == "10.0.0.1"));
        assert!(rows
            .iter()
            .any(|r| r.label == "Team" && r.value == "Platform"));
        assert!(rows
            .iter()
            .any(|r| r.label == "/dev/xvda" && r.value.contains("vol-test")));
        assert!(!rows.iter().any(|r| r.value.contains("null")));
    }
}
