name := libinput-rs
version := 0.2.0
builddir := $(shell mktemp -d)

.PHONY: srpm

srpm:
	dnf -y install cargo rust git gcc make rpm-build
	mkdir -p $(outdir)
	git archive --format=tar.gz --prefix=$(name)-$(version)/ --output=$(builddir)/$(name)-$(version).tar.gz HEAD
	cp $(builddir)/$(name)-$(version).tar.gz $(outdir)/
	rpmbuild -bs $(spec) \
		--define "_sourcedir $(outdir)" \
		--define "_srcrpmdir $(outdir)"
