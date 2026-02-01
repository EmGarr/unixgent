.PHONY: check build test fmt clippy fmt-fix clean docker-test docker-check

check: fmt clippy test

build:
	cargo build --workspace

test:
	cargo test --workspace

fmt:
	cargo fmt --all -- --check

clippy:
	cargo clippy --workspace -- -D warnings

fmt-fix:
	cargo fmt --all

clean:
	cargo clean

docker-test:
	docker build -f Dockerfile.test -t unixagent-test .

docker-check: docker-test
