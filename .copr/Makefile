.DEFAULT_GOAL := srpm

repo_root := $(shell git rev-parse --show-toplevel)
spec ?= $(repo_root)/libinput-rs.spec
outdir ?= $(CURDIR)/out

.PHONY: srpm

srpm:
	dnf -y install git || :
	@set -efu; \
	set -f; \
	repo_root='$(repo_root)'; \
	spec='$(spec)'; \
	outdir='$(outdir)'; \
	if test -n "$$(git -C "$$repo_root" status --porcelain --untracked-files=all --ignore-submodules=all)"; then \
		printf '%s\n' 'refusing to build an SRPM from a dirty checkout' >&2; \
		exit 1; \
	fi; \
	test -f "$$spec"; \
	spec=$$(cd "$$(dirname "$$spec")" && pwd -P)/$$(basename "$$spec"); \
	git -C "$$repo_root" ls-files --error-unmatch -- "$$spec" >/dev/null; \
	metadata=$$(rpmspec -q --srpm --qf '%{NAME} %{VERSION}' "$$spec"); \
	set -- $$metadata; \
	if test "$$#" -ne 2 || test -z "$$1" || test -z "$$2"; then \
		printf '%s\n' 'could not derive a source package name and version from the spec' >&2; \
		exit 1; \
	fi; \
	name=$$1; \
	version=$$2; \
	mkdir -p "$$outdir"; \
	outdir=$$(cd "$$outdir" && pwd -P); \
	archive="$$outdir/$$name-$$version.tar.gz"; \
	if test -e "$$archive"; then \
		printf 'refusing to overwrite existing source archive: %s\n' "$$archive" >&2; \
		exit 1; \
	fi; \
	git -C "$$repo_root" archive --format=tar.gz --prefix="$$name-$$version/" --output="$$archive" HEAD; \
	rpmbuild -bs "$$spec" \
		--define "_sourcedir $$outdir" \
		--define "_srcrpmdir $$outdir"; \
	srpm=$$(find "$$outdir" -maxdepth 1 -type f -name "$$name-$$version-*.src.rpm" -print -quit); \
	test -n "$$srpm"; \
	rpm -qp --qf '%{NAME} %{VERSION}\n' "$$srpm" | grep -Fx "$$name $$version" >/dev/null; \
	printf 'Wrote %s\n' "$$srpm"
