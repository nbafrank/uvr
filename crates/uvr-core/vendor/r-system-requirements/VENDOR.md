# Vendored: rstudio/r-system-requirements

**Upstream:** https://github.com/rstudio/r-system-requirements
**Pin:** `c0506da8f47e601e766a06894da0148f9939a8c1` (2026-04-21)
**License:** MIT (see `LICENSE`)

## Why vendored

Posit's public sysreqs API (`packagemanager.posit.co/__api__/repos/1/sysreqs`)
does not serve Alpine (and possibly other distributions). When the API replies
with `{"code":14,"error":"Unsupported system"}`, `uvr-core` falls back to the
rules in `rules/`, which cover Alpine and every other distro rstudio lists.

## Updating

```sh
cd /tmp && rm -rf r-system-requirements
git clone --depth 1 https://github.com/rstudio/r-system-requirements.git
cp r-system-requirements/rules/*.json \
   path/to/uvr-core/vendor/r-system-requirements/rules/
cp r-system-requirements/systems.json r-system-requirements/LICENSE \
   path/to/uvr-core/vendor/r-system-requirements/
# update the Pin: SHA above, then:
cargo test -p uvr-core
```
