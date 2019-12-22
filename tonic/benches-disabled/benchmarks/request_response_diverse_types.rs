use criterion::*;

use crate::benchmarks::compiled_protos::diverse_types::{GoogleMessage1, GoogleMessage1SubMessage};
use crate::benchmarks::utils;

fn build_request(_name: String) {
    let sub_message = GoogleMessage1SubMessage {
        field1: 10,
        field2: 20,
        field3: 30,
        field15: _name,
        field12: false,
        field13: 70,
        field14: 80,
        field16: 90,
        field19: 100,
        field20: true,
        field28: false,
        field21: 110,
        field22: 120,
        field23: false,
        field206: true,
        field203: 233,
        field204: 333,
        field205: String::from("idiopathic"),
        field207: 4000,
        field300: 4000,
    };

    let _request = tonic::Request::new(GoogleMessage1 {
        field1: String::from("foo"),
        field9: String::from("red"),
        field18: String::from("red"),
        field80: true,
        field81: true,
        field2: 10,
        field3: 30,
        field280: 28,
        field6: 60,
        field22: 220,
        field4: String::from("red"),
        field5: Vec::new(),
        field59: true,
        field7: String::from("blue"),
        field16: 160,
        field130: 13,
        field17: false,
        field12: true,
        field13: true,
        field14: false,
        field104: 1040,
        field100: 50,
        field101: 1010,
        field102: String::from("green"),
        field103: String::from("pink"),
        field29: 290,
        field30: true,
        field60: 601,
        field271: 27,
        field272: 200,
        field150: 15,
        field23: 230,
        field24: false,
        field25: 250,
        field15: Some(sub_message),
        field78: true,
        field67: 670,
        field68: 680,
        field128: 1280,
        field129: String::from("red"),
        field131: 300,
    });
}

pub fn bench_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("Request_Response_Diverse_Types");

    //log plot to get everything on the graph
    let plot_config = PlotConfiguration::default().summary_scale(AxisScale::Logarithmic);

    group.plot_config(plot_config);

    let tiny_string = utils::generate_rnd_string(100).unwrap();
    let short_string = utils::generate_rnd_string(1_000).unwrap();
    let medium_string = utils::generate_rnd_string(10_000).unwrap();

    for size in [tiny_string, short_string, medium_string].iter() {
        group.throughput(Throughput::Bytes(size.len() as u64));

        group.bench_with_input(BenchmarkId::new("request", size.len()), size, |b, i| {
            b.iter(|| build_request(i.to_string()))
        });
    }
    group.finish();
}
