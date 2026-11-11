use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use criterion::{
    BatchSize, BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main,
};
use rotalog::config::{Config, Rotation};
use rotalog::writer::Writer;
use tempfile::TempDir;

const T0: i64 = 1_700_000_000;

fn null_writer(local_time: bool) -> Writer {
    let config = Config {
        logfile: PathBuf::from("/dev/null"),
        rotation: Rotation {
            interval_secs: Some(86_400),
            size_bytes: None,
        },
        truncate: true,
        local_time,
        ..Config::default()
    };
    Writer::new(config).unwrap()
}

fn write_chunks(c: &mut Criterion) {
    let mut group = c.benchmark_group("writer/write");

    for size in [64usize, 1_024, 64 * 1_024] {
        let data = vec![b'x'; size];
        let mut writer = null_writer(false);
        writer.open_initial(T0).unwrap();
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| writer.write(black_box(data.as_slice()), T0).unwrap());
        });
    }

    let mut local = null_writer(true);
    local.open_initial(T0).unwrap();
    let data = vec![b'x'; 64 * 1_024];
    group.throughput(Throughput::Bytes(data.len() as u64));
    group.bench_function("local_time_64KiB", |b| {
        b.iter(|| local.write(black_box(data.as_slice()), T0).unwrap());
    });

    group.finish();
}

fn size_rotations(c: &mut Criterion) {
    let mut group = c.benchmark_group("writer/size_rotation");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(5));

    for (label, limit) in [
        ("64KiB", 64_u64 * 1_024),
        ("1MiB", 1_024 * 1_024),
        ("64MiB", 64 * 1_024 * 1_024),
    ] {
        let data = Arc::new(vec![b'x'; limit as usize]);
        group.throughput(Throughput::Bytes(limit + 1));
        group.bench_with_input(BenchmarkId::from_parameter(label), &limit, |b, &limit| {
            let data = Arc::clone(&data);
            b.iter_batched(
                || {
                    let dir = TempDir::new().unwrap();
                    let config = Config {
                        logfile: dir.path().join("access.log"),
                        rotation: Rotation {
                            interval_secs: None,
                            size_bytes: Some(limit),
                        },
                        ..Config::default()
                    };
                    let writer = Writer::new(config).unwrap();
                    (dir, writer, Arc::clone(&data))
                },
                |(dir, mut writer, data)| {
                    writer.write(black_box(data.as_slice()), T0).unwrap();
                    writer.write(black_box(&b"x"[..]), T0 + 1).unwrap();
                    writer.finish().unwrap();
                    black_box(dir);
                },
                BatchSize::PerIteration,
            );
        });
    }

    group.finish();
}

criterion_group!(benches, write_chunks, size_rotations);
criterion_main!(benches);
