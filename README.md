<p align="center">
  <img src="assets/salus-logo.svg" alt="salus logo" width="720">
</p>

# salus

`salus` is a Rust health check tool for Docker and Kubernetes workloads. It provides a single-shot probe execution model that fits container health checks cleanly.

Supported probe types:

- `http` / `https`
- `tcp`
- `grpc` (standard `grpc.health.v1.Health/Check` only)
- `exec`
- `file`

## Design Goals

- Single execution with stable exit codes for Docker `HEALTHCHECK` and Kubernetes `exec` probes
- Works in minimal container images without requiring a shell
- Supports strict TLS, custom CAs, client certificates, and SNI / hostname overrides
- Failure output is optimized for troubleshooting, while successful probes stay quiet by default

## Architecture

`salus` is easiest to understand as a runtime adapter between the container platform and the probe target inside the workload.

```mermaid
flowchart LR
    subgraph Platform["Container platform"]
        Docker["Docker HEALTHCHECK"]
        K8s["Kubernetes exec probe"]
    end

    subgraph Container["Application container"]
        Salus["salus"]
        HttpTarget["HTTP / HTTPS endpoint"]
        TcpTarget["TCP listener / Unix socket"]
        GrpcTarget["gRPC health service"]
        FileTarget["State / readiness file"]
        ExecTarget["Local validation command"]
    end

    Docker --> Salus
    K8s --> Salus

    Salus --> HttpTarget
    Salus --> TcpTarget
    Salus --> GrpcTarget
    Salus --> FileTarget
    Salus --> ExecTarget

    Salus --> Result["Exit code 0 / 1 / 3 / 4"]
    Result --> Docker
    Result --> K8s
```

## Exit Codes

- `0`: healthy
- `1`: probe failure
- `3`: invalid arguments or configuration
- `4`: internal error

## Examples

HTTP:

```bash
salus http --url http://127.0.0.1:8080/healthz
salus http --url http://127.0.0.1:8080/healthz --header x-api-key:secret
salus http --url https://127.0.0.1:8443/ready --ca /etc/ssl/health-ca.pem --server-name localhost
salus http --url https://127.0.0.1:8443/ready --ca /etc/ssl/health-ca.pem --server-name localhost --header-contains x-ready:ok --contains ready
```

TCP:

```bash
salus tcp --addr 127.0.0.1:5432
```

gRPC health:

```bash
salus grpc --addr 127.0.0.1:50051
salus grpc --addr 127.0.0.1:50051 --tls --ca /etc/ssl/grpc-ca.pem --server-name localhost
```

Exec:

```bash
salus exec --out-contains ok -- /app/bin/check-ready
```

File:

```bash
salus file --path /tmp/ready --non-empty --contains ready
```

## Docker

The production Dockerfile builds a static musl binary and runs it from `scratch`.
Published images are pushed to `ghcr.io/lvillis/salus:<tag>` and stable tags also update `ghcr.io/lvillis/salus:latest`.

```dockerfile
HEALTHCHECK --interval=10s --timeout=3s --retries=3 CMD ["/bin/salus", "http", "--url", "http://127.0.0.1:8080/healthz"]
```

Copy `salus` into an application image:

```dockerfile
FROM ghcr.io/lvillis/salus:latest AS salus

FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=salus /bin/salus /bin/salus
COPY ./my-app /bin/my-app

HEALTHCHECK --interval=10s --timeout=3s --retries=3 CMD ["/bin/salus", "http", "--url", "http://127.0.0.1:8080/healthz", "--contains", "ok"]

ENTRYPOINT ["/bin/my-app"]
```

`salus` expands `${VAR}` and `${VAR:-default}` inside JSON-array arguments before parsing them, so Docker `HEALTHCHECK CMD [...]` does not need `/bin/sh` just to inject environment variables:

```dockerfile
HEALTHCHECK --interval=10s --timeout=3s --retries=3 CMD ["/bin/salus", "http", "--url", "http://127.0.0.1:${PORT}/healthz", "--contains", "ok"]
```

## Kubernetes

Prefer native `httpGet`, `tcpSocket`, and `grpc` probes for simple cases. Use `exec` with `salus` when you need stricter TLS controls, file checks, process-based checks, or richer assertions.

```yaml
livenessProbe:
  exec:
    command:
      - /bin/salus
      - grpc
      - --addr
      - 127.0.0.1:50051
      - --tls
      - --ca
      - /etc/tls/ca.pem
      - --server-name
      - localhost
```

The same `${VAR}` and `${VAR:-default}` expansion works in Kubernetes `exec.command` arrays without relying on a shell inside the container.
