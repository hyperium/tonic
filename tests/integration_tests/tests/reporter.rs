use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use futures_util::FutureExt;
use integration_tests::pb::{test_client, test_server, Input, Output};
use metrics::{GaugeValue, Key, Recorder, Unit};
use tokio::sync::oneshot;
use tonic::{transport::Server, Code, Request, Response, Status};

#[tokio::test]
async fn generates_metrics() {
    struct Svc;

    #[tonic::async_trait]
    impl test_server::Test for Svc {
        async fn unary_call(&self, _: Request<Input>) -> Result<Response<Output>, Status> {
            Err(Status::with_details(
                Code::ResourceExhausted,
                "Too many requests",
                Bytes::from_static(&[1]),
            ))
        }
    }

    struct TestRecorderInner {
        counters: Vec<(Key, u64)>,
        histograms: Vec<(Key, f64)>,
    }

    struct TestRecorder {
        inner: Arc<Mutex<TestRecorderInner>>,
    }

    impl Recorder for TestRecorder {
        fn register_counter(
            &self,
            _key: Key,
            _unit: Option<Unit>,
            _description: Option<&'static str>,
        ) {
        }

        fn register_gauge(
            &self,
            _key: Key,
            _unit: Option<Unit>,
            _description: Option<&'static str>,
        ) {
        }

        fn register_histogram(
            &self,
            _key: Key,
            _unit: Option<Unit>,
            _description: Option<&'static str>,
        ) {
        }

        fn increment_counter(&self, key: Key, value: u64) {
            let mut inner = self.inner.lock().unwrap();
            inner.counters.push((key, value));
        }

        fn update_gauge(&self, _key: Key, _value: GaugeValue) {}

        fn record_histogram(&self, key: Key, value: f64) {
            let mut inner = self.inner.lock().unwrap();
            inner.histograms.push((key, value));
        }
    }

    let recorder_inner = Arc::new(Mutex::new(TestRecorderInner {
        counters: Vec::new(),
        histograms: Vec::new(),
    }));

    let recorder = TestRecorder {
        inner: recorder_inner.clone(),
    };

    metrics::set_boxed_recorder(Box::new(recorder)).unwrap();

    let svc = test_server::TestServer::with_reporter(Svc, tonic_metrics::metrics_reporter_fn);

    let (tx, rx) = oneshot::channel::<()>();

    let jh = tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_shutdown("127.0.0.1:1399".parse().unwrap(), rx.map(drop))
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut channel = test_client::TestClient::connect("http://127.0.0.1:1399")
        .await
        .unwrap();

    let _ = channel
        .unary_call(Request::new(Input {}))
        .await
        .unwrap_err();

    tx.send(()).unwrap();

    jh.await.unwrap();

    let inner = recorder_inner.lock().unwrap();

    assert_eq!(inner.counters.len(), 3);

    let expected_labels = vec![
        ("grpc_method", "unary_call"),
        ("grpc_service", "test.Test"),
        ("grpc_type", "unary"),
    ];

    let counter = &inner.counters[0];
    assert_eq!(format!("{}", counter.0.name()), "grpc_server_started_total");
    let mut labels = counter
        .0
        .labels()
        .map(|l| (l.key(), l.value()))
        .collect::<Vec<_>>();
    labels.sort();
    assert_eq!(labels, expected_labels);
    assert_eq!(counter.1, 1);

    let counter = &inner.counters[1];
    assert_eq!(
        format!("{}", counter.0.name()),
        "grpc_server_msg_received_total"
    );
    let mut labels = counter
        .0
        .labels()
        .map(|l| (l.key(), l.value()))
        .collect::<Vec<_>>();
    labels.sort();
    assert_eq!(labels, expected_labels);
    assert_eq!(counter.1, 1);

    let counter = &inner.counters[2];
    assert_eq!(format!("{}", counter.0.name()), "grpc_server_handled_total");
    let mut labels = counter
        .0
        .labels()
        .map(|l| (l.key(), l.value()))
        .collect::<Vec<_>>();
    labels.sort();
    let expected_labels_with_code = {
        let mut new_expected_labels = expected_labels.clone();
        new_expected_labels.insert(0, ("grpc_code", "ResourceExhausted"));
        new_expected_labels
    };
    assert_eq!(labels, expected_labels_with_code);
    assert_eq!(counter.1, 1);

    assert_eq!(inner.histograms.len(), 1);
    let histogram = &inner.histograms[0];
    assert_eq!(
        format!("{}", histogram.0.name()),
        "grpc_server_handling_seconds"
    );
    let mut labels = histogram
        .0
        .labels()
        .map(|l| (l.key(), l.value()))
        .collect::<Vec<_>>();
    labels.sort();
    assert_eq!(labels, expected_labels);
    assert!(histogram.1 > 0.0);
}
