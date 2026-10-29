# OpenWrt package for monokulo-bell, the Monokular Bell, with the LuCI
# page that manages it (Services > Bell).
#
# The binary is cross-compiled with cargo (scripts/build-openwrt.sh, in the
# same build as monokulo's) and dropped into files/ before the SDK runs, as
# openwrt/monokulo is. What its init script and LuCI page share with the
# Bell's is in ../common.
#
# MONOKULO_VERSION, MONOKULO_RELEASE and MONOKULO_COMMIT come from the
# build script (the crate version, the commit count so every build upgrades
# the last, and the commit it was built from).

include $(TOPDIR)/rules.mk

PKG_NAME:=monokulo-bell
PKG_VERSION:=$(or $(MONOKULO_VERSION),0.1.0)
PKG_RELEASE:=$(or $(MONOKULO_RELEASE),1)
PKG_COMMIT:=$(MONOKULO_COMMIT)

include $(INCLUDE_DIR)/package.mk

COMMON:=$(CURDIR)/../common

define Package/monokulo-bell
  SECTION:=net
  CATEGORY:=Network
  SUBMENU:=Web Servers/Proxies
  TITLE:=Monokular Bell: point of sale payments as dings to its tills
  # A static binary. ca-bundle: TLS to Square, Clover and the tills' push
  # services checks certificates against the system's CA store.
  DEPENDS:=@(aarch64||x86_64) +ca-bundle
  # The Bell runs as this user; created on install by the package scripts.
  USERID:=monokulo-bell:monokulo-bell
endef

define Package/monokulo-bell/description
  The Monokular Bell: it takes a point of sale's payment webhooks (Square,
  Clover) and dings the tills registered at that location with a Web
  Push, so a Monokular till hears of a payment at once. It keeps only the
  tills' registrations. Serves plain HTTP: put an https proxy in front.
  Includes its LuCI page under Services > Bell.
endef

define Package/monokulo-bell/conffiles
/etc/config/monokulo-bell
endef

define Build/Compile
endef

define Package/monokulo-bell/install
	$(INSTALL_DIR) $(1)/usr/bin $(1)/etc/init.d $(1)/etc/config $(1)/etc/uci-defaults $(1)/lib/upgrade/keep.d
	$(INSTALL_BIN) $(CURDIR)/files/monokulo-bell $(1)/usr/bin/monokulo-bell
	$(INSTALL_BIN) $(CURDIR)/files/monokulo-bell.init $(1)/etc/init.d/monokulo-bell
	$(INSTALL_CONF) $(CURDIR)/files/monokulo-bell.config $(1)/etc/config/monokulo-bell
	$(INSTALL_BIN) $(CURDIR)/files/monokulo-bell.defaults $(1)/etc/uci-defaults/90-monokulo-bell
	$(INSTALL_DATA) $(CURDIR)/files/monokulo-bell.keep $(1)/lib/upgrade/keep.d/monokulo-bell
	$(INSTALL_DIR) $(1)/usr/share/monokulo-bell
	$(INSTALL_DATA) $(COMMON)/service.sh $(1)/usr/share/monokulo-bell/service.sh
	# Which build this is: the init script hands it to procd with the
	# instance it starts, so LuCI shows the version that is running.
	printf 'version=%s\ncommit=%s\n' '$(PKG_VERSION)-r$(PKG_RELEASE)' '$(PKG_COMMIT)' \
		>$(1)/usr/share/monokulo-bell/version
	chmod 0644 $(1)/usr/share/monokulo-bell/version
	$(INSTALL_DIR) $(1)/usr/share/rpcd/ucode
	$(INSTALL_DATA) $(COMMON)/rpcd.uc $(1)/usr/share/rpcd/ucode/monokulo-bell
	$(INSTALL_DIR) $(1)/www/luci-static/resources/view
	$(INSTALL_DATA) $(CURDIR)/files/luci/monokulo-bell.js $(1)/www/luci-static/resources/view/monokulo-bell.js
	$(INSTALL_DIR) $(1)/usr/share/luci/menu.d $(1)/usr/share/rpcd/acl.d
	$(INSTALL_DATA) $(CURDIR)/files/luci/menu.json $(1)/usr/share/luci/menu.d/luci-app-monokulo-bell.json
	$(INSTALL_DATA) $(CURDIR)/files/luci/acl.json $(1)/usr/share/rpcd/acl.d/luci-app-monokulo-bell.json
endef

# The VAPID key is made here, so it exists (to back up) from the start.
# LuCI reads its menu and ACLs at login, and rpcd its plugins at start:
# drop LuCI's caches and reload rpcd so Services > Bell shows up (or
# goes) without a reboot.
#
# On an upgrade, restart the Bell so the new binary runs: the SDK's own
# script (default_postinst, which runs before this) only calls start, and
# procd leaves a running instance alone when its command line is unchanged.
define Package/monokulo-bell/postinst
#!/bin/sh
[ -n "$${IPKG_INSTROOT}" ] && exit 0
/etc/init.d/monokulo-bell secrets
rm -f /tmp/luci-indexcache* /tmp/luci-modulecache/* 2>/dev/null
/etc/init.d/rpcd reload 2>/dev/null
if [ "$${PKG_UPGRADE}" = 1 ] && /etc/init.d/monokulo-bell running; then
	/etc/init.d/monokulo-bell restart
fi
exit 0
endef

# Removal leaves the data folder, the secrets and the settings' backup
# (the operator's: the install page says how to delete them), and takes
# the firewall rule the package made.
define Package/monokulo-bell/postrm
#!/bin/sh
[ -n "$${IPKG_INSTROOT}" ] && exit 0
if [ "$${PKG_UPGRADE}" != 1 ] && uci -q get firewall.monokulo_bell >/dev/null; then
	uci -q delete firewall.monokulo_bell && uci -q commit firewall
fi
rm -f /tmp/luci-indexcache* /tmp/luci-modulecache/* /tmp/monokulo-bell.check /tmp/monokulo-bell.blocked 2>/dev/null
/etc/init.d/rpcd reload 2>/dev/null
exit 0
endef

$(eval $(call BuildPackage,monokulo-bell))
