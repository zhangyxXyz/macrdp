//! Regression checks for the vendored clipboard lock and timeout changes.
#[cfg(test)]
mod tests {
    use ironrdp_cliprdr::backend::CliprdrBackend;
    use ironrdp_cliprdr::pdu::*;
    use ironrdp_cliprdr::{CliprdrClient, CliprdrState};
    use ironrdp_core::{AsAny, Encode};
    use ironrdp_svc::{SvcMessage, SvcProcessor};
    use std::sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    };

    #[derive(Debug, Default)]
    struct Backend {
        clock: Arc<AtomicU64>,
        failures: Arc<AtomicUsize>,
        requests: Arc<AtomicUsize>,
        lists: Arc<std::sync::Mutex<Vec<Option<u32>>>>,
        retain: bool,
    }
    impl AsAny for Backend {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }
    impl CliprdrBackend for Backend {
        fn temporary_directory(&self) -> &str {
            "/tmp"
        }
        fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
            flags()
        }
        fn on_ready(&mut self) {}
        fn retain_remote_file_lists(&self) -> bool {
            self.retain
        }
        fn on_remote_file_list(&mut self, _: &[FileDescriptor], id: Option<u32>) {
            self.lists.lock().unwrap().push(id);
        }
        fn on_request_format_list(&mut self) {}
        fn on_process_negotiated_capabilities(&mut self, _: ClipboardGeneralCapabilityFlags) {}
        fn on_remote_copy(&mut self, _: &[ClipboardFormat]) {}
        fn on_format_data_request(&mut self, _: FormatDataRequest) {
            self.requests.fetch_add(1, Ordering::Relaxed);
        }
        fn on_format_data_response(&mut self, _: FormatDataResponse<'_>) {}
        fn on_file_contents_request(&mut self, _: FileContentsRequest) {
            self.requests.fetch_add(1, Ordering::Relaxed);
        }
        fn on_file_contents_response(&mut self, response: FileContentsResponse<'_>) {
            if response.is_error() {
                self.failures.fetch_add(1, Ordering::Relaxed);
            }
        }
        fn on_lock(&mut self, _: LockDataId) {}
        fn on_unlock(&mut self, _: LockDataId) {}
        fn now_ms(&self) -> u64 {
            self.clock.load(Ordering::Relaxed)
        }
        fn elapsed_ms(&self, since: u64) -> u64 {
            self.now_ms().saturating_sub(since)
        }
    }
    fn flags() -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::CAN_LOCK_CLIPDATA
            | ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED
            | ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES
    }
    fn ready() -> (CliprdrClient, Arc<AtomicU64>, Arc<AtomicUsize>) {
        let backend = Backend::default();
        let clock = backend.clock.clone();
        let failures = backend.failures.clone();
        let mut client = CliprdrClient::new(Box::new(backend));
        *client.__test_state_mut() = CliprdrState::Ready;
        *client.__test_capabilities_mut() =
            Capabilities::new(ClipboardProtocolVersion::V2, flags());
        (client, clock, failures)
    }
    fn encoded(pdu: ClipboardPdu<'_>) -> Vec<u8> {
        let mut bytes = vec![0; pdu.size()];
        pdu.encode(&mut ironrdp_core::WriteCursor::new(&mut bytes))
            .unwrap();
        bytes
    }
    fn announce(client: &mut CliprdrClient) -> Option<u32> {
        let formats = [ClipboardFormat {
            id: ClipboardFormatId(49171),
            name: Some(ClipboardFormatName::new("FileGroupDescriptorW")),
        }];
        let format = FormatList::new_unicode(&formats, true).unwrap();
        client
            .process(&encoded(ClipboardPdu::FormatList(format)))
            .unwrap();
        client.__test_current_lock_id()
    }
    fn list(client: &mut CliprdrClient, size: u64) {
        client.initiate_paste(ClipboardFormatId(49171)).unwrap();
        let list = PackedFileList {
            files: vec![FileDescriptor::new("same.bin").with_file_size(size)],
        };
        client
            .process(&encoded(ClipboardPdu::FormatDataResponse(
                FormatDataResponse::new_file_list(&list).unwrap(),
            )))
            .unwrap();
    }
    fn request(sid: u32, lock: Option<u32>, offset: u64) -> FileContentsRequest {
        FileContentsRequest {
            stream_id: sid,
            index: 0,
            flags: FileContentsFlags::RANGE,
            position: offset,
            requested_size: 1,
            data_id: lock,
        }
    }
    fn sent_lock(message: &SvcMessage) -> Option<u32> {
        let bytes = message.encode_unframed_pdu().unwrap();
        match ironrdp_core::decode::<ClipboardPdu<'_>>(&bytes).unwrap() {
            ClipboardPdu::FileContentsRequest(req) => req.data_id,
            _ => panic!("expected request"),
        }
    }
    #[test]
    fn original_lock_is_preserved_only_when_explicitly_supplied() {
        let (mut client, _, _) = ready();
        let first = announce(&mut client).unwrap();
        list(&mut client, 10);
        let second = announce(&mut client).unwrap();
        list(&mut client, 20);
        assert_ne!(first, second);
        let implicit: Vec<SvcMessage> = client
            .request_file_contents(request(1, None, 0))
            .unwrap()
            .into();
        let explicit: Vec<SvcMessage> = client
            .request_file_contents(request(2, Some(first), 0))
            .unwrap()
            .into();
        assert_eq!(sent_lock(&implicit[0]), Some(second));
        assert_eq!(sent_lock(&explicit[0]), Some(first));
    }
    #[test]
    fn explicit_old_lock_validates_against_its_own_file_size() {
        let (mut client, _, failures) = ready();
        let first = announce(&mut client).unwrap();
        list(&mut client, 10);
        announce(&mut client);
        list(&mut client, 20);
        assert!(client
            .request_file_contents(request(1, Some(first), 15))
            .is_err());
        assert_eq!(failures.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn candidate_unknown_lock_is_rejected() {
        let (mut client, _, _) = ready();
        announce(&mut client);
        list(&mut client, 20);
        assert!(
            client
                .request_file_contents(request(1, Some(0xdead), 0))
                .is_err(),
            "pin changed; re-evaluate unknown-lock gap"
        );
    }
    #[test]
    fn unleased_locks_are_released_on_local_file_copy() {
        let (mut client, _, _) = ready();
        let lock = announce(&mut client).unwrap();
        list(&mut client, 20);
        client
            .request_file_contents(request(1, Some(lock), 0))
            .unwrap();
        assert_eq!(client.__test_sent_file_contents_requests().len(), 1);
        client
            .initiate_file_copy(vec![FileDescriptor::new("local.txt")])
            .unwrap();
        assert!(
            client.__test_outgoing_locks().is_empty(),
            "pin changed; re-evaluate local-copy gap"
        );
    }
    #[test]
    fn without_timer_lock_capacity_is_bounded() {
        let (mut client, clock, _) = ready();
        for i in 0..100 {
            clock.store(i * 1000, Ordering::Relaxed);
            assert!(announce(&mut client).is_some());
        }
        clock.store(10_000_000, Ordering::Relaxed);
        assert!(announce(&mut client).is_none());
        assert_eq!(client.__test_outgoing_locks().len(), 100);
        // Merely advancing time does nothing. Driving the existing sweep frees
        // expired locks and allows the next copy to obtain one again.
        client.drive_timeouts().unwrap();
        assert!(client.__test_outgoing_locks().is_empty());
        assert!(announce(&mut client).is_some());
    }
    #[test]
    fn driven_timeout_releases_waiting_range_request_via_error_callback() {
        let (mut client, clock, failures) = ready();
        let lock = announce(&mut client).unwrap();
        list(&mut client, 20);
        client
            .request_file_contents(request(1, Some(lock), 0))
            .unwrap();
        clock.store(61_000, Ordering::Relaxed);
        client.drive_timeouts().unwrap();
        assert_eq!(failures.load(Ordering::Relaxed), 1);
        assert!(client.__test_sent_file_contents_requests().is_empty());
    }
    #[test]
    fn candidate_incoming_unknown_lock_never_reaches_backend() {
        let backend = Backend::default();
        let requests = backend.requests.clone();
        let mut client = CliprdrClient::new(Box::new(backend));
        *client.__test_state_mut() = CliprdrState::Ready;
        *client.__test_capabilities_mut() =
            Capabilities::new(ClipboardProtocolVersion::V2, flags());
        *client.__test_local_file_list_mut() = Some(PackedFileList {
            files: vec![FileDescriptor::new("current.bin").with_file_size(20)],
        });
        client
            .process(&encoded(ClipboardPdu::FileContentsRequest(request(
                1,
                Some(0xdead),
                0,
            ))))
            .unwrap();
        assert_eq!(
            requests.load(Ordering::Relaxed),
            0,
            "pin changed; re-evaluate incoming unknown-lock fallback"
        );
    }
    #[test]
    fn candidate_explicit_lease_survives_local_copy_then_releases() {
        let (mut client, clock, _) = ready();
        let lock = announce(&mut client).unwrap();
        list(&mut client, 20);
        assert!(client.retain_remote_lock(lock));
        client
            .initiate_file_copy(vec![FileDescriptor::new("local.txt")])
            .unwrap();
        clock.store(61_000, Ordering::Relaxed);
        client.drive_timeouts().unwrap();
        assert!(client.__test_outgoing_locks().contains_key(&lock));
        client.release_remote_lock(lock);
        client.drive_timeouts().unwrap();
        assert!(!client.__test_outgoing_locks().contains_key(&lock));
    }
    #[test]
    fn candidate_lease_has_hard_maximum_lifetime() {
        let (mut client, clock, _) = ready();
        let lock = announce(&mut client).unwrap();
        list(&mut client, 20);
        assert!(client.retain_remote_lock(lock));
        announce(&mut client);
        clock.store(7_201_000, Ordering::Relaxed);
        client.drive_timeouts().unwrap();
        assert!(!client.__test_outgoing_locks().contains_key(&lock));
    }

    #[test]
    fn delayed_file_list_uses_its_original_request_lock() {
        let backend = Backend::default();
        let callbacks = backend.lists.clone();
        let mut client = CliprdrClient::new(Box::new(backend));
        *client.__test_state_mut() = CliprdrState::Ready;
        *client.__test_capabilities_mut() =
            Capabilities::new(ClipboardProtocolVersion::V2, flags());
        let first = announce(&mut client).unwrap();
        client.initiate_paste(ClipboardFormatId(49171)).unwrap();
        let second = announce(&mut client).unwrap();
        let files = PackedFileList {
            files: vec![FileDescriptor::new("old.bin").with_file_size(5)],
        };
        client
            .process(&encoded(ClipboardPdu::FormatDataResponse(
                FormatDataResponse::new_file_list(&files).unwrap(),
            )))
            .unwrap();
        assert_eq!(*callbacks.lock().unwrap(), vec![Some(first)]);
        list(&mut client, 20);
        assert_eq!(*callbacks.lock().unwrap(), vec![Some(first), Some(second)]);
        let explicit: Vec<SvcMessage> = client
            .request_file_contents(request(1, Some(first), 0))
            .unwrap()
            .into();
        assert_eq!(sent_lock(&explicit[0]), Some(first));
    }
    #[test]
    fn backend_leases_are_installed_before_local_copy_can_release_locks() {
        let backend = Backend {
            retain: true,
            ..Backend::default()
        };
        let mut client = CliprdrClient::new(Box::new(backend));
        *client.__test_state_mut() = CliprdrState::Ready;
        *client.__test_capabilities_mut() =
            Capabilities::new(ClipboardProtocolVersion::V2, flags());
        let lock = announce(&mut client).unwrap();
        list(&mut client, 20);
        client
            .initiate_file_copy(vec![FileDescriptor::new("local.bin")])
            .unwrap();
        assert!(client.__test_outgoing_locks().contains_key(&lock));
        client.release_remote_lock(lock);
    }
    #[test]
    fn multiple_consumers_of_one_lock_release_independently() {
        let (mut client, clock, _) = ready();
        let lock = announce(&mut client).unwrap();
        list(&mut client, 20);
        assert!(client.retain_remote_lock(lock));
        assert!(client.retain_remote_lock(lock));
        client.release_remote_lock(lock);
        client
            .initiate_file_copy(vec![FileDescriptor::new("local")])
            .unwrap();
        clock.store(61_000, Ordering::Relaxed);
        client.drive_timeouts().unwrap();
        assert!(client.__test_outgoing_locks().contains_key(&lock));
        client.release_remote_lock(lock);
        client.drive_timeouts().unwrap();
        assert!(!client.__test_outgoing_locks().contains_key(&lock));
    }
    #[test]
    fn asynchronous_format_reads_and_inline_drop_effect_keep_wire_order() {
        let backend = Backend::default();
        let requests = backend.requests.clone();
        let mut client = CliprdrClient::new(Box::new(backend));
        *client.__test_state_mut() = CliprdrState::Ready;
        *client.__test_capabilities_mut() =
            Capabilities::new(ClipboardProtocolVersion::V2, flags());
        client
            .initiate_file_copy(vec![FileDescriptor::new("test")])
            .unwrap();
        let request = |id| {
            encoded(ClipboardPdu::FormatDataRequest(FormatDataRequest {
                format: ClipboardFormatId::new(id),
            }))
        };
        assert!(client.process(&request(13)).unwrap().is_empty());
        assert!(client.process(&request(0xc0fd)).unwrap().is_empty());
        assert!(client.process(&request(8)).unwrap().is_empty());
        assert_eq!(requests.load(Ordering::Relaxed), 1);
        let replies: Vec<SvcMessage> = client
            .submit_format_data(OwnedFormatDataResponse::new_error())
            .unwrap()
            .into();
        assert_eq!(replies.len(), 2);
        let first = replies[0].encode_unframed_pdu().unwrap();
        let second = replies[1].encode_unframed_pdu().unwrap();
        assert!(
            matches!(ironrdp_core::decode::<ClipboardPdu<'_>>(&first).unwrap(), ClipboardPdu::FormatDataResponse(r) if r.is_error())
        );
        assert!(
            matches!(ironrdp_core::decode::<ClipboardPdu<'_>>(&second).unwrap(), ClipboardPdu::FormatDataResponse(r) if r.data() == 1u32.to_le_bytes())
        );
        assert_eq!(requests.load(Ordering::Relaxed), 2);
        client
            .submit_format_data(OwnedFormatDataResponse::new_error())
            .unwrap();
    }
}
