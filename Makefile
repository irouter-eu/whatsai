.PHONY: build test check smoke worker-smoke
build:
	cargo build --workspace --locked
	npm --prefix adapters ci
	npm --prefix adapters run build
check:
	cargo fmt --all --check
	cargo clippy --workspace --all-targets --locked -- -D warnings
	npm --prefix adapters run build
test:
	cargo test --workspace --locked
	npm --prefix adapters test
smoke:
	python3 scripts/smoke.py
worker-smoke:
	python3 scripts/worker-smoke.py
