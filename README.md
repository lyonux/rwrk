# rwrk

A distributed load testing tool with built-in Prometheus metrics.

## Architecture

rwrk uses a **controller/worker** architecture for distributed load testing:

```
                  ┌──────────────┐
                  │  enroll      │  Configuration Service
                  │   :8081      │  (SSE push)
                  └──────┬───────┘
                         │
            ┌────────────┼────────────┐
            │            │            │
       ┌────▼───┐   ┌───▼────┐  ┌───▼────┐
       │ rwrk   │   │ rwrk   │  │ rwrk   │  Workers
       │ :9090  │   │ :9090  │  │ :9090  │  (metrics)
       └────┬───┘   └───┬────┘  └───┬────┘
            │            │            │
            └────────────┼────────────┘
                         │
              ┌──────────▼──────────┐
              │   Target Service    │
              │          pon        │
              └─────────────────────┘
```

- **enroll** — Configuration service. Workers register and receive test parameters via Server-Sent Events. Dynamically adjust connection count, request rate, and target host at runtime.
- **rwrk** — Load test worker. Connects to the enrollment service, generates HTTP traffic, and exposes Prometheus metrics on `:9090/metrics`.
- **pon** — HTTP echo server for use as a load test target. Provides `/check`, `/ping`, `/post`, and `/ws` endpoints.
- **rgrpc** — gRPC echo service for use as a load test target.
- **echo** — Shared HTTP handler library used by `pon`.

## Quick Start

Build the workspace:

```bash
cargo build --workspace
```

Start each component in separate terminals:

```bash
# 1. Start the configuration service
cargo run -p enroll -- -l 8081

# 2. Start a target HTTP server
cargo run -p pon -- -l 8080

# 3. Start a load test worker
cargo run -p rwrk -- -c "http://localhost:8081/get?id=rwrk"
```

Configure the test by sending parameters to the enrollment service:

```bash
curl "http://localhost:8081/set?id=rwrk&conn=100&host=http://localhost:8080&rate=1000&expire=60"
```

View Prometheus metrics:

```bash
curl http://localhost:9090/metrics
```

## Key Features

- **Dynamic configuration** — Adjust connection count, target host, request rate, and test duration at runtime via SSE.
- **Prometheus metrics** — Built-in metrics endpoint tracking request counts, status codes, and latency histograms.
- **Token bucket rate limiting** — Precise request rate control per worker.
- **Connection pooling** — Efficient HTTP client reuse across concurrent requests.
- **WebSocket support** — Test WebSocket endpoints via the `/ws` echo handler.

## License

[MIT](LICENSE)
