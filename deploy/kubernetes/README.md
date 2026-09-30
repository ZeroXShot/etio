# Kubernetes

A standalone deployment: one StatefulSet with a persistent volume for
snapshots and history, bearer tokens from a Secret, and a hardened pod
(non-root, read-only root filesystem, no capabilities).

No image is published yet: build it (`docker build -t <registry>/etio:0.1.0 .`),
push it to a registry your cluster can pull from, and set `images` in
`kustomization.yaml` accordingly.

```sh
kubectl create secret generic etio-tokens \
  --from-literal=ingest="$(openssl rand -hex 32)" \
  --from-literal=read="$(openssl rand -hex 32)"
kubectl apply -k deploy/kubernetes
```

Point your OpenTelemetry Collector at `etio:4317` with the ingest token:

```yaml
exporters:
  otlp/etio:
    endpoint: etio.<namespace>.svc:4317
    tls: { insecure: true }     # or enable [tls] in etio.toml
    headers:
      authorization: "Bearer ${env:ETIO_INGEST_TOKEN}"
```

For the distributed mode, run edges as a Deployment behind the Collector's
`loadbalancing` exporter (with the `k8s` or `dns` resolver on a headless
Service) and cores as a StatefulSet; see `docs/operations.md`.
