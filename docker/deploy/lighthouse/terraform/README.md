# A hosted lighthouse, by Terraform

Three root modules that stand up one `sharerr-lighthouse` on a cloud
free tier (or the nearest thing to it), for a friend group that would rather
not run one of their own. All three run the image GHCR publishes and nothing
else; the two VM targets run the exact compose layout in the parent directory,
`compose.yaml` plus `compose.tls.yaml` when a domain is given.

| Target | What you get | TLS | Cost, as of writing |
| --- | --- | --- | --- |
| [`aws/`](aws/) | One t3.micro, Elastic IP, Docker via cloud-init | Caddy, with your domain | Free tier: 750 h/month for twelve months on older accounts, a credit allowance on newer ones |
| [`azure/`](azure/) | One B1s VM, static IP, Docker via cloud-init | Caddy, with your domain | Free account: 750 h/month for twelve months; the Standard-SKU public IP is a few dollars a month on top |
| [`fly/`](fly/) | One shared-cpu-1x machine with a 1 GB volume, via flyctl | Built in, at `<app>.fly.dev` | No free allowance any more; two to three dollars a month |

Fly is the least work and the only one with TLS on day one. The VMs are the
only ones that can be actually free, for a year, and need a domain for TLS
(without one they serve plain HTTP on 7878, which works but leaves the
lookup answer only as trustworthy as the wire it travels).

## Before you start

- Terraform 1.6+ or OpenTofu.
- For `aws/`: credentials in the environment or `~/.aws`.
- For `azure/`: `az login`, and a subscription id (`az account show`) in
  `ARM_SUBSCRIPTION_ID` or the `subscription_id` variable.
- For `fly/`: [flyctl](https://fly.io/docs/flyctl/install/), logged in.
- For TLS on a VM: a hostname you control. Apply first, then point its A
  record at the address in the outputs; Caddy keeps retrying the certificate
  until DNS resolves.

## Use

```bash
cd aws            # or azure, or fly
terraform init
terraform apply -var domain=lighthouse.example.net   # VM targets: omit for plain HTTP
terraform output lighthouse_url
```

Then each friend puts that URL in `lighthouse.urls` (Settings → Lighthouse),
and points their own instance's `lighthouse.urls` at it too so it can report
its endpoint. `terraform destroy` removes everything, Elastic IP included —
an idle Elastic IP is the one thing here that bills while nothing runs.

Azure requires an SSH public key (`-var "ssh_public_key=$(cat
~/.ssh/id_ed25519.pub)"`); AWS takes one optionally and opens port 22 only
when given. Narrow `ssh_cidr` to your own address either way.

## What runs on the box

The VM targets render one cloud-init document (`modules/cloud-init/`) that
installs Ubuntu's own `docker.io` and `docker-compose-v2` packages, writes the
compose files from the parent directory verbatim into
`/opt/sharerr-lighthouse/`, brings them up, and installs a daily cron that
pulls the image and recreates the container if `:latest` moved. Nothing is
curl-piped into a shell. There is no state worth backing up: `/data` holds a
single decoy secret, and losing it reshuffles fabricated answers after a
restart, not a credential.

## What is deliberately not here

- **A public instance's address.** This directory is how to run one, not
  where one runs. If the project ever hosts an instance, its URL belongs in
  [`docs/LIGHTHOUSE.md`](../../../../docs/LIGHTHOUSE.md).
- **Remote state.** Three resources per target is not worth a backend; keep
  the state file wherever you keep the rest of your homelab's.
- **Azure Container Apps, AWS Fargate, Lightsail.** Considered. Container
  Apps' always-free grant does not cover an always-on replica, Fargate has no
  free tier, and Lightsail's is a three-month trial.
