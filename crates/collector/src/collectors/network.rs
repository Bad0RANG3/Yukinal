//! Network 采集器：`/proc/net/dev` 的 rx/tx 字节矩阵。

use futures::future::BoxFuture;

use crate::{run, CollectedData, Collector, CollectorContext, CollectorError, Result};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkSample {
    pub name: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

pub struct Network;

impl Collector for Network {
    fn id(&self) -> &'static str {
        "network"
    }

    fn detect(&self, context: &CollectorContext) -> BoxFuture<'static, Result<bool>> {
        let context = context.clone_context();
        Box::pin(async move {
            let out = run(&context, "test -r /proc/net/dev && echo ok").await?;
            Ok(out.exit_code == 0)
        })
    }

    fn collect(&self, context: &CollectorContext) -> BoxFuture<'static, Result<CollectedData>> {
        let context = context.clone_context();
        Box::pin(async move {
            let out = run(&context, "cat /proc/net/dev").await?;
            let samples = parse_net_dev(&out.stdout);
            if samples.is_empty() {
                return Err(CollectorError::Collect {
                    collector: "network".into(),
                    message: "no interfaces in /proc/net/dev".into(),
                });
            }
            Ok(CollectedData::Network(samples))
        })
    }
}

fn parse_net_dev(raw: &str) -> Vec<NetworkSample> {
    let mut samples = Vec::new();
    for line in raw.lines().skip(2) {
        // eth0:  rx  tx ... , rx bytes is field 1, tx bytes field 9
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let fields: Vec<u64> = rest
            .split_whitespace()
            .map(|field| field.parse().unwrap_or(0))
            .collect();
        if fields.len() >= 9 {
            samples.push(NetworkSample {
                name: name.trim().to_string(),
                rx_bytes: fields[0],
                tx_bytes: fields[8],
            });
        }
    }
    samples
}

#[cfg(test)]
mod tests {
    use super::*;

    const NET_DEV_SAMPLE: &str = "Inter-|   Receive                                                |  Transmit\n face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n  eth0: 1099511627776  123456789    0    0    0     0          0         0  2199023255552  987654321    0    0    0     0     0          0\n    lo:       10000        10    0    0    0     0          0         0      10000        10    0    0    0     0     0          0\n";

    #[test]
    fn net_dev_parses_bytes_matrix() {
        let samples = parse_net_dev(NET_DEV_SAMPLE);
        assert_eq!(samples.len(), 2);
        let eth = &samples[0];
        assert_eq!(eth.name, "eth0");
        assert_eq!(eth.rx_bytes, 1_099_511_627_776);
        assert_eq!(eth.tx_bytes, 2_199_023_255_552);
    }
}
