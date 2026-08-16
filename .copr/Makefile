# Makefile for Copr's SCM "make srpm" build method.
#
# Copr requires this exact file (.copr/Makefile, relative to the repo root)
# for the "make_srpm" SRPM build method. It clones this repo at the tagged
# ref and invokes it like this (see
# https://docs.copr.fedorainfracloud.org/user_documentation.html#make-srpm):
#
#   make -f <cloned_repodir>/.copr/Makefile srpm outdir="<outdir>" spec="<spec_path>"
#
# This runs entirely on Copr's own Fedora build infrastructure, in a mock
# chroot that HAS internet access (unlike the isolated %build phase that
# follows). This target vendors Cargo dependencies fresh on every build and
# produces an SRPM — no separate CI step and no pre-uploaded tarball needed.
#
# `outdir` and `spec` are passed in by Copr as command-line make variables
# and override the defaults below.
#
# Optional local sanity check (not required to ship the package — Copr
# builds it independently):
#   make -f .copr/Makefile srpm
# Result lands in outdir/. Requires rpmbuild + cargo wherever you run this.

NAME    := makima-deckery
VERSION := $(shell awk -F'"' '/^version/{print $$2; exit}' $(CURDIR)/Cargo.toml)
outdir  ?= $(CURDIR)/outdir
spec    ?= packaging/$(NAME).spec

.PHONY: srpm vendor builddeps clean

# Copr's srpm-generation chroot is minimal — it does NOT contain the spec's
# %build BuildRequires (those only apply to the later, isolated %build
# phase). Per the make_srpm docs, this script runs with root privileges and
# is expected to install whatever it needs itself. Guarded by `command -v
# dnf` so this is a no-op wherever dnf isn't available and cargo/git/
# rpm-build are expected to already be present.
builddeps:
	command -v dnf >/dev/null 2>&1 && ! command -v cargo >/dev/null 2>&1 \
		&& dnf -y install cargo rust git rpm-build || true

srpm: builddeps vendor
	mkdir -p $(outdir)
	git archive --prefix=$(NAME)-$(VERSION)/ -o $(outdir)/$(NAME)-$(VERSION).tar HEAD
	gzip -f $(outdir)/$(NAME)-$(VERSION).tar
	rpmbuild -bs \
		--define "_sourcedir $(outdir)" \
		--define "_srcrpmdir $(outdir)" \
		--define "_topdir $(outdir)/rpmbuild" \
		$(spec)
	@echo "SRPM written to $(outdir)"

vendor: builddeps
	mkdir -p $(outdir)
	# `cargo vendor`'s stdout is not just noise — it's the actual required
	# .cargo/config.toml content, including a [source."git-url"] override
	# for every git dependency (we have one: the evdev fork). A hand-typed
	# config.toml in the spec only covers crates-io and silently breaks any
	# git dependency, since Cargo then tries to clone it live in the
	# network-isolated %build chroot. So: capture it and ship it, don't
	# guess it.
	cargo vendor > cargo-config.toml
	tar czf $(outdir)/$(NAME)-$(VERSION)-vendor.tar.gz vendor/ cargo-config.toml

clean:
	rm -rf $(outdir) vendor
