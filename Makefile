COMPOSE_DIR   := docker/compose
TEST_DIR      := docker/tests
IMAGE_NAME    := pim:latest

# ── Build ──────────────────────────────────────────────────────────────────────

.PHONY: docker-build
docker-build:
	docker build -t $(IMAGE_NAME) .

# ── Test labs ─────────────────────────────────────────────────────────────────
# Lab names describe what they exercise. The historical phase numbering
# (p1..p8) tracked the implementation plan and is preserved as a comment
# next to each target for traceability.

.PHONY: test-single-hop test-single-hop-ipv6 test-multi-hop test-peer-discovery \
        test-resilience test-resilience-full test-multi-gateway test-auto-discovery \
        test-auto-ip-chain test-auth test-debug-cli test-route-cli \
        test-bluetooth test-bluetooth-enx test-all

test-single-hop: docker-build         ## phase 1: TUN, NAT, gateway/client baseline
	bash $(TEST_DIR)/test-single-hop.sh

test-single-hop-ipv6: docker-build    ## phase 1 IPv6 variant
	bash $(TEST_DIR)/test-ipv6.sh

test-multi-hop: docker-build          ## phase 2: relay forwarding + routing + failover
	bash $(TEST_DIR)/test-multi-hop.sh

test-peer-discovery: docker-build     ## phase 3: UDP-broadcast peer lifecycle
	bash $(TEST_DIR)/test-peer-discovery.sh

test-resilience: docker-build         ## phase 4: reconnect + flow control (skip 6 min NAT timeout)
	SKIP_SLOW=1 bash $(TEST_DIR)/test-resilience.sh

test-resilience-full: docker-build    ## phase 4 including the 6 min NAT-timeout test
	SKIP_SLOW=0 bash $(TEST_DIR)/test-resilience.sh

test-multi-gateway: docker-build      ## phase 5: multi-gateway failover + load
	bash $(TEST_DIR)/test-multi-gateway.sh

test-auto-discovery: docker-build     ## phase 7: zero-config discovery + chain
	bash $(TEST_DIR)/test-auto-discovery.sh

test-auto-ip-chain: docker-build      ## phase 8: routed auto-IP chain with late gateway join
	bash $(TEST_DIR)/test-auto-ip-chain.sh

test-auth: docker-build
	bash $(TEST_DIR)/test-authorization.sh

test-debug-cli: docker-build
	bash $(TEST_DIR)/test-debug-cli.sh

test-route-cli: docker-build
	bash $(TEST_DIR)/test-route-cli.sh

test-bluetooth: docker-build
	bash $(TEST_DIR)/test-bluetooth.sh

test-bluetooth-enx: docker-build
	bash $(TEST_DIR)/test-bluetooth-enx.sh

test-all: docker-build
	@bash $(TEST_DIR)/test-single-hop.sh && \
	 bash $(TEST_DIR)/test-ipv6.sh && \
	 bash $(TEST_DIR)/test-multi-hop.sh && \
	 bash $(TEST_DIR)/test-peer-discovery.sh && \
	 SKIP_SLOW=1 bash $(TEST_DIR)/test-resilience.sh && \
	 bash $(TEST_DIR)/test-multi-gateway.sh && \
	 bash $(TEST_DIR)/test-authorization.sh && \
	 bash $(TEST_DIR)/test-route-cli.sh && \
	 bash $(TEST_DIR)/test-bluetooth.sh && \
	 bash $(TEST_DIR)/test-bluetooth-enx.sh

# ── Manual stack management ───────────────────────────────────────────────────
# Use these for interactive debugging without the test scripts.

.PHONY: up-single-hop up-single-hop-ipv6 up-multi-hop-relay up-multi-hop-routing \
        up-peer-discovery up-resilience up-flow-control up-multi-gateway \
        up-auto-discovery up-auto-ip-chain up-bluetooth up-bluetooth-enx

up-single-hop:
	docker compose -f $(COMPOSE_DIR)/single-hop.yml up -d --build

up-single-hop-ipv6:
	docker compose -f $(COMPOSE_DIR)/single-hop-ipv6.yml up -d --build

up-multi-hop-relay:
	docker compose -f $(COMPOSE_DIR)/multi-hop-relay.yml up -d --build

up-multi-hop-routing:
	docker compose -f $(COMPOSE_DIR)/multi-hop-routing.yml up -d --build

up-peer-discovery:
	docker compose -f $(COMPOSE_DIR)/peer-discovery.yml up -d --build

up-resilience:
	docker compose -f $(COMPOSE_DIR)/resilience.yml up -d --build

up-flow-control:
	docker compose -f $(COMPOSE_DIR)/flow-control.yml up -d --build

up-multi-gateway:
	docker compose -f $(COMPOSE_DIR)/multi-gateway.yml up -d --build

up-auto-discovery:
	docker compose -f $(COMPOSE_DIR)/auto-discovery.yml up -d --build

up-auto-ip-chain:
	docker compose -f $(COMPOSE_DIR)/auto-ip-chain.yml up -d --build

up-bluetooth:
	docker compose -f $(COMPOSE_DIR)/bluetooth-seam.yml up -d --build

up-bluetooth-enx:
	docker compose -f $(COMPOSE_DIR)/bluetooth-seam-enx.yml up -d --build

.PHONY: down-single-hop down-single-hop-ipv6 down-multi-hop-relay down-multi-hop-routing \
        down-peer-discovery down-resilience down-flow-control down-multi-gateway \
        down-auto-discovery down-auto-ip-chain down-bluetooth down-bluetooth-enx

down-single-hop:
	docker compose -f $(COMPOSE_DIR)/single-hop.yml down -v --remove-orphans

down-single-hop-ipv6:
	docker compose -f $(COMPOSE_DIR)/single-hop-ipv6.yml down -v --remove-orphans

down-multi-hop-relay:
	docker compose -f $(COMPOSE_DIR)/multi-hop-relay.yml down -v --remove-orphans

down-multi-hop-routing:
	docker compose -f $(COMPOSE_DIR)/multi-hop-routing.yml down -v --remove-orphans

down-peer-discovery:
	docker compose -f $(COMPOSE_DIR)/peer-discovery.yml down -v --remove-orphans

down-resilience:
	docker compose -f $(COMPOSE_DIR)/resilience.yml down -v --remove-orphans

down-flow-control:
	docker compose -f $(COMPOSE_DIR)/flow-control.yml down -v --remove-orphans

down-multi-gateway:
	docker compose -f $(COMPOSE_DIR)/multi-gateway.yml down -v --remove-orphans

down-auto-discovery:
	docker compose -f $(COMPOSE_DIR)/auto-discovery.yml down -v --remove-orphans

down-auto-ip-chain:
	docker compose -f $(COMPOSE_DIR)/auto-ip-chain.yml down -v --remove-orphans

down-bluetooth:
	docker compose -f $(COMPOSE_DIR)/bluetooth-seam.yml down -v --remove-orphans

down-bluetooth-enx:
	docker compose -f $(COMPOSE_DIR)/bluetooth-seam-enx.yml down -v --remove-orphans

# ── Log tailing ───────────────────────────────────────────────────────────────

.PHONY: logs-single-hop logs-single-hop-ipv6 logs-multi-hop-relay logs-multi-hop-routing \
        logs-peer-discovery logs-resilience logs-multi-gateway logs-auto-discovery \
        logs-auto-ip-chain logs-bluetooth logs-bluetooth-enx

logs-single-hop:
	docker compose -f $(COMPOSE_DIR)/single-hop.yml logs -f

logs-single-hop-ipv6:
	docker compose -f $(COMPOSE_DIR)/single-hop-ipv6.yml logs -f

logs-multi-hop-relay:
	docker compose -f $(COMPOSE_DIR)/multi-hop-relay.yml logs -f

logs-multi-hop-routing:
	docker compose -f $(COMPOSE_DIR)/multi-hop-routing.yml logs -f

logs-peer-discovery:
	docker compose -f $(COMPOSE_DIR)/peer-discovery.yml logs -f

logs-resilience:
	docker compose -f $(COMPOSE_DIR)/resilience.yml logs -f

logs-multi-gateway:
	docker compose -f $(COMPOSE_DIR)/multi-gateway.yml logs -f

logs-auto-discovery:
	docker compose -f $(COMPOSE_DIR)/auto-discovery.yml logs -f

logs-auto-ip-chain:
	docker compose -f $(COMPOSE_DIR)/auto-ip-chain.yml logs -f

logs-bluetooth:
	docker compose -f $(COMPOSE_DIR)/bluetooth-seam.yml logs -f

logs-bluetooth-enx:
	docker compose -f $(COMPOSE_DIR)/bluetooth-seam-enx.yml logs -f

# ── Shortcuts for exec ────────────────────────────────────────────────────────

.PHONY: sh-single-hop-gw sh-single-hop-client sh-multi-hop-relay sh-multi-hop-client sh-multi-gateway-relay

sh-single-hop-gw:
	docker exec -it pim-single-hop-gw bash

sh-single-hop-client:
	docker exec -it pim-single-hop-client bash

sh-multi-hop-relay:
	docker exec -it pim-multi-hop-relay-relay bash

sh-multi-hop-client:
	docker exec -it pim-multi-hop-relay-client bash

sh-multi-gateway-relay:
	docker exec -it pim-multi-gateway-relay bash

# ── Clean ─────────────────────────────────────────────────────────────────────

.PHONY: docker-clean clean-all

docker-clean:
	@for f in \
	    $(COMPOSE_DIR)/single-hop.yml \
	    $(COMPOSE_DIR)/single-hop-ipv6.yml \
	    $(COMPOSE_DIR)/multi-hop-relay.yml \
	    $(COMPOSE_DIR)/multi-hop-routing.yml \
	    $(COMPOSE_DIR)/peer-discovery.yml \
	    $(COMPOSE_DIR)/resilience.yml \
	    $(COMPOSE_DIR)/flow-control.yml \
	    $(COMPOSE_DIR)/multi-gateway.yml \
	    $(COMPOSE_DIR)/auto-discovery.yml \
	    $(COMPOSE_DIR)/auto-ip-chain.yml \
	    $(COMPOSE_DIR)/auth-allow-all.yml \
	    $(COMPOSE_DIR)/auth-allow-list.yml \
	    $(COMPOSE_DIR)/auth-tofu.yml \
	    $(COMPOSE_DIR)/auth-discovery-key.yml \
	    $(COMPOSE_DIR)/bluetooth-seam.yml \
	    $(COMPOSE_DIR)/bluetooth-seam-enx.yml; do \
	    docker compose -f $$f down -v --remove-orphans 2>/dev/null || true; \
	done

clean-all: docker-clean
	docker rmi $(IMAGE_NAME) 2>/dev/null || true
	docker image prune -f 2>/dev/null || true

# ── Unit tests (non-Docker) ───────────────────────────────────────────────────

.PHONY: test-unit
test-unit:
	cargo test --workspace

.PHONY: help
help:
	@echo "PIM Docker test targets:"
	@echo ""
	@echo "  make docker-build           Build pim:latest image"
	@echo "  make test-single-hop        TUN, NAT, gateway/client baseline (phase 1)"
	@echo "  make test-single-hop-ipv6   IPv6 single-hop + NAT66 + split-default routes"
	@echo "  make test-multi-hop         Relay + routing + failover (phase 2)"
	@echo "  make test-peer-discovery    Discovery + peer lifecycle (phase 3)"
	@echo "  make test-resilience        Resilience + flow control (phase 4, SKIP_SLOW=1)"
	@echo "  make test-resilience-full   Phase 4 including the 6-min NAT timeout test"
	@echo "  make test-multi-gateway     Multi-gateway + failover + load (phase 5)"
	@echo "  make test-auto-discovery    Zero-config auto-discovery (phase 7)"
	@echo "  make test-auto-ip-chain     Auto-IP chain + late gateway join (phase 8)"
	@echo "  make test-auth              Authorization policies + keyed discovery"
	@echo "  make test-debug-cli         Debug CLI output in multi-gateway and discovery labs"
	@echo "  make test-route-cli         Split-default route CLI flow in the single-hop Docker lab"
	@echo "  make test-bluetooth         Bluetooth fake-sysfs seam test in Docker"
	@echo "  make test-bluetooth-enx     Bluetooth dynamic enx PAN fallback seam test in Docker"
	@echo "  make test-all               All labs (slow tests skipped)"
	@echo "  make test-unit              Rust unit tests (no Docker)"
	@echo ""
	@echo "  make up-single-hop          Start single-hop stack (no tests)"
	@echo "  make up-single-hop-ipv6     Start IPv6 single-hop stack"
	@echo "  make down-single-hop        Stop single-hop stack"
	@echo "  make down-single-hop-ipv6   Stop IPv6 single-hop stack"
	@echo "  make logs-single-hop        Follow single-hop logs"
	@echo "  make logs-single-hop-ipv6   Follow IPv6 single-hop logs"
	@echo "  make sh-single-hop-client   Shell into single-hop client container"
	@echo "  make up-bluetooth           Start Bluetooth seam stack"
	@echo "  make down-bluetooth         Stop Bluetooth seam stack"
	@echo "  make logs-bluetooth         Follow Bluetooth seam logs"
	@echo ""
	@echo "  make docker-clean           Stop and remove all stacks"
	@echo "  make clean-all              docker-clean + remove image"
