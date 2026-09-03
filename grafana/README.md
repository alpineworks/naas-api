# Grafana dashboard

`dashboard.json` is the operational dashboard for naas-api. Panels:

| Row | Panels |
|---|---|
| Header | Pool fill % • entropy push rate (5m) • bytes served rate (5m) • reader errors (1h) |
| Throughput | Entropy push rate per device • Pool depth (pending vs capacity) |
| Output | Bytes served by format (stacked) • Health-check rejections per device |
| Errors | Reader errors per device (full-width bar) |

Variables: `datasource` (Prometheus, defaults to UID `prom`/Mimir), `device` (multi-select from `naas_entropy_bytes_total`).

## Import

### Via UI
Grafana → Dashboards → New → Import → upload `dashboard.json`. Select the Prometheus datasource when prompted.

### Via API
```sh
DS_UID=prom  # adjust to your Prometheus/Mimir datasource UID
curl -fsS -u admin:"$GRAFANA_ADMIN_PASSWORD" \
  -H 'Content-Type: application/json' \
  -X POST https://grafana.example/api/dashboards/db \
  -d "$(jq --arg uid "$DS_UID" '
    {dashboard: ., overwrite: true, inputs: [{name:"DS_PROMETHEUS",type:"datasource",pluginId:"prometheus",value:$uid}]}
  ' dashboard.json)"
```

## Provisioning via the lgtm-distributed Helm chart

If you later enable the Grafana sidecar (`grafana.sidecar.dashboards.enabled: true`), drop this JSON into a ConfigMap labeled `grafana_dashboard: "1"` in the same namespace as Grafana — it will auto-load on the next sync. The naas-api repo is the source of truth; the ConfigMap can `kustomize build` against this file.
