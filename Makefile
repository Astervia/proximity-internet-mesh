COMPOSE_DIR   := docker/compose
TEST_DIR      := docker/tests
IMAGE_NAME    := pim:latest

# ── Build ──────────────────────────────────────────────────────────────────────

.PHONY: docker-build
docker-build:
	docker build -t $(IMAGE_NAME) .

# ── Test phases ───────────────────────────────────────────────────────────────

.PHONY: test-p1 test-p2 test-p3 test-p4 test-p5 test-p7 test-debug-cli test-route-cli test-bluetooth test-all

test-p1: docker-build
	bash $(TEST_DIR)/test-phase1.sh

test-p2: docker-build
	bash $(TEST_DIR)/test-phase2.sh

test-p3: docker-build
	bash $(TEST_DIR)/test-phase3.sh

test-p4: docker-build
	SKIP_SLOW=1 bash $(TEST_DIR)/test-phase4.sh

# Skip the 6-minute NAT timeout test by default; set SKIP_SLOW=0 to enable.
test-p4-full: docker-build
	SKIP_SLOW=0 bash $(TEST_DIR)/test-phase4.sh

test-p5: docker-build
	bash $(TEST_DIR)/test-phase5.sh

test-p7: docker-build
	bash $(TEST_DIR)/test-phase7.sh

test-debug-cli: docker-build
	bash $(TEST_DIR)/test-debug-cli.sh

test-route-cli: docker-build
	bash $(TEST_DIR)/test-route-cli.sh

test-bluetooth: docker-build
	bash $(TEST_DIR)/test-bluetooth.sh

test-all: docker-build
	@bash $(TEST_DIR)/test-phase1.sh && \
	 bash $(TEST_DIR)/test-phase2.sh && \
	 bash $(TEST_DIR)/test-phase3.sh && \
	 SKIP_SLOW=1 bash $(TEST_DIR)/test-phase4.sh && \
	 bash $(TEST_DIR)/test-phase5.sh && \
	 bash $(TEST_DIR)/test-route-cli.sh && \
	 bash $(TEST_DIR)/test-bluetooth.sh

# ── Manual stack management ───────────────────────────────────────────────────
# Use these for interactive debugging without the test scripts.

.PHONY: up-p1 up-p2-relay up-p2-routing up-p3 up-p4 up-p4-fc up-p5 up-p7 up-bluetooth

up-p1:
	docker compose -f $(COMPOSE_DIR)/phase1-single-hop.yml up -d --build

up-p2-relay:
	docker compose -f $(COMPOSE_DIR)/phase2-relay.yml up -d --build

up-p2-routing:
	docker compose -f $(COMPOSE_DIR)/phase2-routing.yml up -d --build

up-p3:
	docker compose -f $(COMPOSE_DIR)/phase3-discovery.yml up -d --build

up-p4:
	docker compose -f $(COMPOSE_DIR)/phase4-resilience.yml up -d --build

up-p4-fc:
	docker compose -f $(COMPOSE_DIR)/phase4-flow-control.yml up -d --build

up-p5:
	docker compose -f $(COMPOSE_DIR)/phase5-multigateway.yml up -d --build

up-p7:
	docker compose -f $(COMPOSE_DIR)/phase7-auto-discovery.yml up -d --build

up-bluetooth:
	docker compose -f $(COMPOSE_DIR)/bluetooth-seam.yml up -d --build

.PHONY: down-p1 down-p2-relay down-p2-routing down-p3 down-p4 down-p4-fc down-p5 down-p7 down-bluetooth

down-p1:
	docker compose -f $(COMPOSE_DIR)/phase1-single-hop.yml down -v --remove-orphans

down-p2-relay:
	docker compose -f $(COMPOSE_DIR)/phase2-relay.yml down -v --remove-orphans

down-p2-routing:
	docker compose -f $(COMPOSE_DIR)/phase2-routing.yml down -v --remove-orphans

down-p3:
	docker compose -f $(COMPOSE_DIR)/phase3-discovery.yml down -v --remove-orphans

down-p4:
	docker compose -f $(COMPOSE_DIR)/phase4-resilience.yml down -v --remove-orphans

down-p4-fc:
	docker compose -f $(COMPOSE_DIR)/phase4-flow-control.yml down -v --remove-orphans

down-p5:
	docker compose -f $(COMPOSE_DIR)/phase5-multigateway.yml down -v --remove-orphans

down-p7:
	docker compose -f $(COMPOSE_DIR)/phase7-auto-discovery.yml down -v --remove-orphans

down-bluetooth:
	docker compose -f $(COMPOSE_DIR)/bluetooth-seam.yml down -v --remove-orphans

# ── Log tailing ───────────────────────────────────────────────────────────────

.PHONY: logs-p1 logs-p2-relay logs-p2-routing logs-p3 logs-p4 logs-p5 logs-p7 logs-bluetooth

logs-p1:
	docker compose -f $(COMPOSE_DIR)/phase1-single-hop.yml logs -f

logs-p2-relay:
	docker compose -f $(COMPOSE_DIR)/phase2-relay.yml logs -f

logs-p2-routing:
	docker compose -f $(COMPOSE_DIR)/phase2-routing.yml logs -f

logs-p3:
	docker compose -f $(COMPOSE_DIR)/phase3-discovery.yml logs -f

logs-p4:
	docker compose -f $(COMPOSE_DIR)/phase4-resilience.yml logs -f

logs-p5:
	docker compose -f $(COMPOSE_DIR)/phase5-multigateway.yml logs -f

logs-p7:
	docker compose -f $(COMPOSE_DIR)/phase7-auto-discovery.yml logs -f

logs-bluetooth:
	docker compose -f $(COMPOSE_DIR)/bluetooth-seam.yml logs -f

# ── Shortcuts for exec ────────────────────────────────────────────────────────
# e.g.:  make sh-p1-client

.PHONY: sh-p1-gateway sh-p1-client sh-p2-relay sh-p2-client sh-p5-relay

sh-p1-gateway:
	docker exec -it pim-p1-gw bash

sh-p1-client:
	docker exec -it pim-p1-client bash

sh-p2-relay:
	docker exec -it pim-p2-relay bash

sh-p2-client:
	docker exec -it pim-p2-client bash

sh-p5-relay:
	docker exec -it pim-p5-relay bash

# ── Clean ─────────────────────────────────────────────────────────────────────

.PHONY: docker-clean clean-all

docker-clean:
	@for f in \
	    $(COMPOSE_DIR)/phase1-single-hop.yml \
	    $(COMPOSE_DIR)/phase2-relay.yml \
	    $(COMPOSE_DIR)/phase2-routing.yml \
	    $(COMPOSE_DIR)/phase3-discovery.yml \
	    $(COMPOSE_DIR)/phase4-resilience.yml \
	    $(COMPOSE_DIR)/phase4-flow-control.yml \
	    $(COMPOSE_DIR)/phase5-multigateway.yml \
	    $(COMPOSE_DIR)/phase7-auto-discovery.yml \
	    $(COMPOSE_DIR)/bluetooth-seam.yml; do \
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
	@echo "  make docker-build       Build pim:latest image"
	@echo "  make test-p1            Phase 1: single-hop tunnel"
	@echo "  make test-p2            Phase 2: relay + routing + failover"
	@echo "  make test-p3            Phase 3: discovery + peer lifecycle"
	@echo "  make test-p4            Phase 4: resilience + flow control (SKIP_SLOW=1)"
	@echo "  make test-p4-full       Phase 4: includes 6-min NAT timeout test"
	@echo "  make test-p5            Phase 5: multi-gateway + failover + load"
	@echo "  make test-p7            Phase 7: zero-config auto-discovery"
	@echo "  make test-debug-cli     Debug CLI output in multi-gateway and discovery labs"
	@echo "  make test-route-cli     Split-default route CLI flow in the single-hop Docker lab"
	@echo "  make test-bluetooth     Bluetooth fake-sysfs seam test in Docker"
	@echo "  make test-all           All phases (slow tests skipped)"
	@echo "  make test-unit          Rust unit tests (no Docker)"
	@echo ""
	@echo "  make up-p1              Start phase 1 stack (no tests)"
	@echo "  make down-p1            Stop phase 1 stack"
	@echo "  make logs-p1            Follow phase 1 logs"
	@echo "  make sh-p1-client       Shell into phase 1 client container"
	@echo "  make up-bluetooth       Start Bluetooth seam stack"
	@echo "  make down-bluetooth     Stop Bluetooth seam stack"
	@echo "  make logs-bluetooth     Follow Bluetooth seam logs"
	@echo ""
	@echo "  make docker-clean       Stop and remove all stacks"
	@echo "  make clean-all          docker-clean + remove image"
