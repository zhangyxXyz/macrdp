//! Optional connect-time measurement from clintcan/macrdp#235, on the
//! negotiated MCS message channel between Client Info and licensing.

use std::time::Duration;

use ironrdp_async::{Framed, FramedRead, FramedWrite};
use ironrdp_core::decode;
use ironrdp_pdu::mcs::SendDataRequest;
use ironrdp_pdu::rdp::autodetect::{
    AutoDetectRequest, AutoDetectResponse, AutoDetectRspPdu, BW_RESULTS_CONNECT_TIME, NETCHAR_RESULT_RTT,
};
use ironrdp_pdu::x224::X224;
use rand::RngCore as _;

use super::{AutoDetectManager, AutoDetectOutcome};
use crate::error::{ServerError, ServerErrorExt as _, ServerResult};
use crate::server::{encode_autodetect_request, monotonic_now_ms};

impl AutoDetectManager {
    /// Measure initial RTT and bandwidth. Both waits share the two-second
    /// deadline. Missing, stale or unrelated replies do not fail the login.
    /// Returns whether a bandwidth response (including a zero duration) or cached metrics arrived.
    pub async fn measure_connect_time<S>(
        &mut self,
        framed: &mut Framed<S>,
        user_channel_id: u16,
        message_channel_id: u16,
    ) -> ServerResult<bool>
    where
        S: FramedRead + FramedWrite,
    {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let mut channel = ProbeChannel {
            framed,
            user_channel_id,
            message_channel_id,
            deadline,
        };
        let seq = self.send_rtt_request(monotonic_now_ms()).sequence_number();
        channel.send(AutoDetectRequest::rtt_connect_time(seq)).await?;
        let response = channel.response(seq, true).await?;
        let (seq, received_bandwidth) = match response {
            Some(ref response @ AutoDetectResponse::RttResponse { .. }) => {
                self.handle_response(response, monotonic_now_ms());
                let seq = self.next_sequence;
                self.next_sequence = seq.wrapping_add(1);
                channel.send(AutoDetectRequest::bw_start_connect_time(seq)).await?;
                // Keep MCS userData below the unfragmented 14-bit PER length.
                for _ in 0..32 {
                    let mut payload = vec![0; 8 * 1024];
                    rand::rng().fill_bytes(&mut payload);
                    channel.send(AutoDetectRequest::bw_payload(seq, payload)).await?;
                }
                channel
                    .send(AutoDetectRequest::bw_stop_connect_time(seq, vec![0; 4]))
                    .await?;
                let response = channel.response(seq, false).await?;
                let received = response.is_some();
                match response {
                    Some(AutoDetectResponse::NetworkCharacteristicsSync {
                        bandwidth_kbps, rtt_ms, ..
                    }) => {
                        self.use_cached_metrics(bandwidth_kbps, rtt_ms);
                    }
                    response => self.bandwidth_kbps = response.and_then(|r| r.computed_bandwidth_kbps()),
                }
                (seq, received)
            }
            Some(AutoDetectResponse::NetworkCharacteristicsSync {
                bandwidth_kbps, rtt_ms, ..
            }) => {
                self.use_cached_metrics(bandwidth_kbps, rtt_ms);
                (seq, true)
            }
            _ => {
                self.pending_probes.clear();
                return Ok(false);
            }
        };
        if let Some(result) = self.network_result(seq) {
            channel.send(result).await?;
        }
        Ok(received_bandwidth)
    }

    fn use_cached_metrics(&mut self, bandwidth_kbps: u32, rtt_ms: u32) {
        self.pending_probes.clear();
        self.rtt_samples.clear();
        self.rtt_samples.push_back(rtt_ms);
        self.min_rtt_ms = Some(rtt_ms);
        self.bandwidth_kbps = Some(bandwidth_kbps);
    }

    fn network_result(&self, sequence_number: u16) -> Option<AutoDetectRequest> {
        let avg = self.snapshot()?.avg_ms;
        let base = self.min_rtt_ms?;
        Some(match self.bandwidth_kbps {
            Some(bw) => AutoDetectRequest::netchar_result(sequence_number, base, bw, avg),
            None => AutoDetectRequest::NetworkCharacteristicsResult {
                sequence_number,
                request_type: NETCHAR_RESULT_RTT,
                base_rtt_ms: Some(base),
                bandwidth_kbps: None,
                average_rtt_ms: avg,
            },
        })
    }

    /// Update RTT on a matching reply and report it with the login bandwidth
    /// estimate. No reply means no new result.
    pub fn handle_continuous_response(
        &mut self,
        response: &AutoDetectResponse,
        now_ms: u64,
    ) -> Option<AutoDetectRequest> {
        if !matches!(self.handle_response(response, now_ms), AutoDetectOutcome::Rtt(_)) {
            return None;
        }
        self.continuous_network_result()
    }

    pub(crate) fn continuous_network_result(&mut self) -> Option<AutoDetectRequest> {
        let seq = self.next_sequence;
        self.next_sequence = seq.wrapping_add(1);
        self.network_result(seq)
    }
}

struct ProbeChannel<'a, S> {
    framed: &'a mut Framed<S>,
    user_channel_id: u16,
    message_channel_id: u16,
    deadline: tokio::time::Instant,
}

impl<S: FramedRead + FramedWrite> ProbeChannel<'_, S> {
    async fn send(&mut self, request: AutoDetectRequest) -> ServerResult<()> {
        let bytes = encode_autodetect_request(request, self.message_channel_id, self.user_channel_id)?;
        self.framed
            .write_all(&bytes)
            .await
            .map_err(|e| ServerError::io("write network detection", e))
    }

    async fn response(&mut self, seq: u16, expect_rtt: bool) -> ServerResult<Option<AutoDetectResponse>> {
        while tokio::time::Instant::now() < self.deadline {
            let Ok(frame) = tokio::time::timeout_at(self.deadline, self.framed.read_pdu()).await else {
                break;
            };
            let (_, bytes) = frame.map_err(|e| ServerError::io("read network detection", e))?;
            let Ok(X224(data)) = decode::<X224<SendDataRequest<'_>>>(&bytes) else {
                continue;
            };
            if data.channel_id != self.message_channel_id {
                continue;
            }
            let Ok(reply) = decode::<AutoDetectRspPdu>(data.user_data.as_ref()) else {
                continue;
            };
            if reply.response.sequence_number() == seq
                && match reply.response {
                    AutoDetectResponse::RttResponse { .. } => expect_rtt,
                    AutoDetectResponse::BandwidthMeasureResults {
                        response_type: BW_RESULTS_CONNECT_TIME,
                        ..
                    } => !expect_rtt,
                    AutoDetectResponse::NetworkCharacteristicsSync { .. } => true,
                    _ => false,
                }
            {
                return Ok(Some(reply.response));
            }
        }
        tracing::debug!("network detection reply timed out, continuing login");
        Ok(None)
    }
}
