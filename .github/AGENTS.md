# CI workflows

Local contract for `.github/` and `.gitea/`, whose `AGENTS.md` links to this one. Extends the root [AGENTS.md](../AGENTS.md).

- A change to a workflow that both `.github/workflows/` and `.gitea/workflows/` carry (`ci.yml`, `ci-image.yml`, `coverage.yml`, `release.yml`) lands in both trees; [build-and-ci.md](../docs/contributing/build-and-ci.md#continuous-integration) says where they differ on purpose. A claim about which CI jobs build a platform first searches both workflow trees and the image build files under each of the platform's spellings (`rg -n -i -l 'arm64|aarch64|musl' .github .gitea docker ci`): the Docker matrix names `linux/arm64` and `ubuntu-24.04-arm` rather than the triple, and `docker-image.yml` builds rimz natively on arm64 at release.
