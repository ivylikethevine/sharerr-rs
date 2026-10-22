# Dashboards

Ready-made views of one sharerr instance, for the three places an operator
already looks. Each reads an endpoint `[metrics]` turns on (see
[`SETTINGS.md`](../SETTINGS.md#metrics)): `/metrics` for Prometheus, and the
`/dashboard` JSON for the widget tools. Both need the vault's `metrics.token`
as a bearer token and answer `404` until `metrics.enabled` is on.

## Contents

- [Grafana](#grafana)
- [Homepage](#homepage)
- [Home Assistant](#home-assistant)

## Grafana

[`grafana.json`](grafana.json) is a dashboard import: _Dashboards → New →
Import_, upload the file, pick the Prometheus data source that scrapes
sharerr. Stat tiles across the top (what is seeding, friends active, the
live swarm, bytes served, the last sync's result and age), time series below
(transfer rate from the tracker's announces, swarm size, items by state,
memory and data-directory use). Every query filters on an `instance`
variable, so several sharerr instances scraped by one Prometheus each get
their own view.

The scrape job it expects:

```yaml
scrape_configs:
  - job_name: sharerr
    scheme: http
    static_configs:
      - targets: ["sharerr:8477"]
    authorization:
      type: Bearer
      credentials_file: /etc/prometheus/sharerr.token
```

## Homepage

[`homepage.yaml`](homepage.yaml) is a service entry for
[Homepage](https://gethomepage.dev)'s `services.yaml`, using its
[Custom API](https://gethomepage.dev/widgets/services/customapi/) widget
over `/dashboard`. The token comes from a `HOMEPAGE_VAR_SHARERR_TOKEN`
environment variable on the Homepage container, so it never sits in the
YAML. The four mappings shown (items, size on disk, active friends, bytes
served) are a choice; every field of the `/dashboard` payload is listed in
[`openapi.json`](../openapi.json) under `DashboardWidget`.

Homarr and Glance read the same endpoint through their own custom-API
widgets; the field names are the same.

## Home Assistant

[`home-assistant.yaml`](home-assistant.yaml) declares the
[`rest`](https://www.home-assistant.io/integrations/rest/) integration over
`/dashboard`: one sensor per field, a timestamp for the last sync and a
`problem` binary sensor that is on when the last pass errored. Put the
token in `secrets.yaml` as the whole header value:

```yaml
sharerr_metrics_token: Bearer <the token>
```

Both the `rest` integration and Homepage poll on a fixed interval, so
`last_sync_at` lags the Status page by up to that interval.
