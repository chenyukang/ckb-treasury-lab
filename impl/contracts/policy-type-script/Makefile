TOP ?= $(abspath ../..)
MODE ?= release
CUSTOM_RUSTFLAGS ?= -C debug-assertions
FULL_RUSTFLAGS := -C target-feature=+zba,+zbb,+zbc,+zbs -C passes=lower-atomic $(CUSTOM_RUSTFLAGS)
MODE_ARGS := $(if $(filter release,$(MODE)),--release,)
CLANG ?= $(shell $(TOP)/scripts/find_clang)
AR := $(subst clang,llvm-ar,$(CLANG))
OBJCOPY := $(subst clang,llvm-objcopy,$(CLANG))
BINARIES := policy-type-script

build:
	RUSTFLAGS="$(FULL_RUSTFLAGS)" TARGET_CC="$(CLANG)" TARGET_AR="$(AR)" cargo build --target=riscv64imac-unknown-none-elf $(MODE_ARGS)
	@if [ -n "$(BUILD_DIR)" ]; then for binary in $(BINARIES); do cp $(TOP)/target/riscv64imac-unknown-none-elf/$(MODE)/$$binary $(TOP)/$(BUILD_DIR)/$$binary; cp $(TOP)/$(BUILD_DIR)/$$binary $(TOP)/$(BUILD_DIR)/$$binary.debug; $(OBJCOPY) --strip-debug --strip-all $(TOP)/$(BUILD_DIR)/$$binary; done; fi

clippy:
	RUSTFLAGS="$(FULL_RUSTFLAGS)" TARGET_CC="$(CLANG)" TARGET_AR="$(AR)" cargo clippy --target=riscv64imac-unknown-none-elf $(MODE_ARGS)

.PHONY: build clippy
