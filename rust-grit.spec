%bcond_without check

Name:           grit
Version:        0.1.0
Release:        %autorelease
Summary:        Fast, native, single-binary Git client with an embedded web UI

License:        MIT
URL:            https://github.com/bit-shift-io/grit
Source0:        %{url}/archive/v%{version}/grit-%{version}.tar.gz

BuildRequires:  cargo-rpm-macros >= 24
BuildRequires:  cargo

%description
Grit is a fast Git client that ships as a single native binary. It runs an
embedded web daemon serving a browser UI over WebSockets; building with the
desktop feature adds a native Iced desktop GUI on top.

%prep
%autosetup -n grit-%{version}
%cargo_prep

%build
%cargo_build

%install
%cargo_install

%if %{with check}
%check
%cargo_test
%endif

%files
%license LICENSE
%doc README.md
%{_bindir}/grit

%changelog
%autochangelog
