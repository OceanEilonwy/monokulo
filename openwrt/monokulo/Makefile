# OpenWrt package for monokulo and its engine, with the LuCI page that runs
# them (Services > Monokulo), as one package.
#
# The two binaries are cross-compiled with cargo (scripts/build-openwrt.sh)
# and dropped into files/ before the SDK runs: building Rust inside the SDK
# would mean bootstrapping a whole Rust toolchain, and monokulo's build also
# needs Node for the POS app.
#
# MONOKULO_VERSION and MONOKULO_RELEASE come from the build script (the
# crate version, and the commit count so every build upgrades the last).

include $(TOPDIR)/rules.mk

PKG_NAME:=monokulo
PKG_VERSION:=$(or $(MONOKULO_VERSION),0.1.0)
PKG_RELEASE:=$(or $(MONOKULO_RELEASE),1)

include $(INCLUDE_DIR)/package.mk

define Package/monokulo
  SECTION:=net
  CATEGORY:=Network
  SUBMENU:=Web Servers/Proxies
  TITLE:=Monokulo: a self-hosted Monero payment gateway
  # Static binaries. ca-bundle: TLS to Monero nodes and exchange rate
  # services checks certificates against the system's CA store. taskset
  # (engine CPU pinning) is a BusyBox applet on OpenWrt 25.12.
  DEPENDS:=@(aarch64) +ca-bundle
  # Both processes run as this user; created on install by the package scripts.
  USERID:=monokulo:monokulo
endef

define Package/monokulo/description
  Accept Monero on your own router. Monokulo watches the chain with each
  store's view key (never a spend key), tracks orders and tells the store
  when one is paid. Two static binaries: monokulo, the web app and admin,
  and monokulo-engine, the private engine that scans the chain. Includes
  its LuCI page under Services > Monokulo.
endef

define Package/monokulo/conffiles
/etc/config/monokulo
endef

define Build/Compile
endef

define Package/monokulo/install
	$(INSTALL_DIR) $(1)/usr/bin $(1)/etc/init.d $(1)/etc/config $(1)/lib/upgrade/keep.d
	$(INSTALL_BIN) $(CURDIR)/files/monokulo $(1)/usr/bin/monokulo
	$(INSTALL_BIN) $(CURDIR)/files/monokulo-engine $(1)/usr/bin/monokulo-engine
	$(INSTALL_DIR) $(1)/usr/libexec/monokulo
	$(INSTALL_BIN) $(CURDIR)/files/monokulo-engine.pinned $(1)/usr/libexec/monokulo/monokulo-engine
	$(INSTALL_BIN) $(CURDIR)/files/monokulo.init $(1)/etc/init.d/monokulo
	$(INSTALL_CONF) $(CURDIR)/files/monokulo.config $(1)/etc/config/monokulo
	$(INSTALL_DATA) $(CURDIR)/files/monokulo.keep $(1)/lib/upgrade/keep.d/monokulo
	$(INSTALL_DIR) $(1)/www/luci-static/resources/view
	$(INSTALL_DATA) $(CURDIR)/files/luci/monokulo.js $(1)/www/luci-static/resources/view/monokulo.js
	$(INSTALL_DIR) $(1)/usr/share/luci/menu.d $(1)/usr/share/rpcd/acl.d
	$(INSTALL_DATA) $(CURDIR)/files/luci/menu.json $(1)/usr/share/luci/menu.d/luci-app-monokulo.json
	$(INSTALL_DATA) $(CURDIR)/files/luci/acl.json $(1)/usr/share/rpcd/acl.d/luci-app-monokulo.json
endef

# LuCI reads its menu and ACLs at login; drop its caches so Services >
# Monokulo shows up (or goes) without a reboot.
define Package/monokulo/postinst
#!/bin/sh
[ -n "$${IPKG_INSTROOT}" ] && exit 0
rm -f /tmp/luci-indexcache* /tmp/luci-modulecache/* 2>/dev/null
/etc/init.d/rpcd reload 2>/dev/null
exit 0
endef

define Package/monokulo/postrm
#!/bin/sh
[ -n "$${IPKG_INSTROOT}" ] && exit 0
rm -f /tmp/luci-indexcache* /tmp/luci-modulecache/* 2>/dev/null
/etc/init.d/rpcd reload 2>/dev/null
exit 0
endef

$(eval $(call BuildPackage,monokulo))
