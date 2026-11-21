# HorizonPad — 常用任务
#
#   make test        全部 Rust 测试
#   make cli         构建 hz 命令行
#   make verify      Windows/任意平台上的端到端验证
#   make ios         macOS：交叉编译 iOS 静态库
#   make app         macOS：生成并打开 Xcode 工程
#   make clean

CARGO ?= cargo
ENGINE := engine
RELEASE := $(ENGINE)/target/release/hz
ifeq ($(OS),Windows_NT)
	RELEASE := $(ENGINE)/target/release/hz.exe
endif

.PHONY: all test cli verify verify-download ios app stubs preflight check-swift fmt lint clean

all: test cli

test:
	$(CARGO) test --manifest-path $(ENGINE)/Cargo.toml

cli:
	$(CARGO) build --release --manifest-path $(ENGINE)/Cargo.toml -p hz-cli

verify: cli
	pwsh -File scripts/verify-pipeline.ps1

ios:
	./scripts/build-ios.sh

app:
	cd app && xcodegen generate && open HorizonPad.xcodeproj

# 为一个 jar 生成 Rust stub 骨架：make stubs JAR=path/to/26.3.jar
JAR ?=
stubs: cli
	@test -n "$(JAR)" || (echo "用法: make stubs JAR=path/to/client.jar" && exit 2)
	$(RELEASE) stubs "$(JAR)" stubs/

# 没有 macOS 也能做的 Swift 检查：
#   swift_check.py    结构（花括号、C 头与封装是否脱节）
#   swift_typecheck   真有 toolchain 时：parse 全部 + 类型检查 FFI 与模型层
# 上云前自检：绝对路径、缺失引用、超大文件
preflight:
	python3 scripts/preflight.py

# 下载链：catalog 断言 + Swift 写出的 zip 能被引擎 unzip/port
verify-download:
	python3 scripts/verify-download-chain.py
	python3 scripts/test-resume.py

check-swift:
	@python3 scripts/swift_check.py || true
	@python3 scripts/swift_refs.py || true
	@if command -v swiftc >/dev/null 2>&1; then bash scripts/swift_typecheck.sh; \
	 else echo "no swiftc on PATH: structure check only"; fi

fmt:
	$(CARGO) fmt --manifest-path $(ENGINE)/Cargo.toml

lint:
	$(CARGO) clippy --manifest-path $(ENGINE)/Cargo.toml -- -D warnings

clean:
	$(CARGO) clean --manifest-path $(ENGINE)/Cargo.toml
	rm -rf app/HorizonPad.xcodeproj app/build