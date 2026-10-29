# OpenWrt package for monokulo-push-relay, the push relay on its own, with
# the LuCI page that manages it (Services > Push relay).
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

PKG_NAME:=monokulo-push-relay
PKG_VERSION:=$(or $(MONOKULO_VERSION),0.1.0)
PKG_RELEASE:=$(or $(MONOKULO_RELEASE),1)
PKG_COMMIT:=$(MONOKULO_COMMIT)

include $(INCLUDE_DIR)/package.mk

COMMON:=$(CURDIR)/../common

define Package/monokulo-push-relay
  SECTION:=net
  CATEGORY:=Network
  SUBMENU:=Web Servers/Proxies
  TITLE:=Monokulo push relay: Web Push to APNs and FCM for till apps
  # A static binary. ca-bundle: TLS to APNs and FCM checks certificates
  # against the system's CA store.
  DEPENDS:=@(aarch64||x86_64) +ca-bundle
  # The relay runs as this user; created on install by the package scripts.
  USERID:=monokulo-push-relay:monokulo-push-relay
endef

define Package/monokulo-push-relay/description
  The push relay of Monokulo, on its own: it takes the Web Push monokulo
  and the Monokular Bell send to a till app and hands it on to APNs or
  FCM, under the app's own keys. It keeps only its handles, never a push.
  Serves plain HTTP: put an https proxy in front. Includes its LuCI page
  under Services > Push relay.
endef

define Package/monokulo-push-relay/conffiles
/etc/config/monokulo-push-relay
endef

define Build/Compile
endef

define Package/monokulo-push-relay/install
	$(INSTALL_DIR) $(1)/usr/bin $(1)/etc/init.d $(1)/etc/config $(1)/etc/uci-defaults $(1)/lib/upgrade/keep.d
	$(INSTALL_BIN) $(CURDIR)/files/monokulo-push-relay $(1)/usr/bin/monokulo-push-relay
	$(INSTALL_BIN) $(CURDIR)/files/monokulo-push-relay.init $(1)/etc/init.d/monokulo-push-relay
	$(INSTALL_CONF) $(CURDIR)/files/monokulo-push-relay.config $(1)/etc/config/monokulo-push-relay
	$(INSTALL_BIN) $(CURDIR)/files/monokulo-push-relay.defaults $(1)/etc/uci-defaults/90-monokulo-push-relay
	$(INSTALL_DATA) $(CURDIR)/files/monokulo-push-relay.keep $(1)/lib/upgrade/keep.d/monokulo-push-relay
	$(INSTALL_DIR) $(1)/usr/share/monokulo-push-relay
	$(INSTALL_DATA) $(COMMON)/service.sh $(1)/usr/share/monokulo-push-relay/service.sh
	# Which build this is: the init script hands it to procd with the
	# instance it starts, so LuCI shows the version that is running.
	printf 'version=%s\ncommit=%s\n' '$(PKG_VERSION)-r$(PKG_RELEASE)' '$(PKG_COMMIT)' \
		>$(1)/usr/share/monokulo-push-relay/version
	chmod 0644 $(1)/usr/share/monokulo-push-relay/version
	$(INSTALL_DIR) $(1)/usr/share/rpcd/ucode
	$(INSTALL_DATA) $(COMMON)/rpcd.uc $(1)/usr/share/rpcd/ucode/monokulo-push-relay
	$(INSTALL_DIR) $(1)/www/luci-static/resources/view
	$(INSTALL_DATA) $(CURDIR)/files/luci/monokulo-push-relay.js $(1)/www/luci-static/resources/view/monokulo-push-relay.js
	$(INSTALL_DIR) $(1)/usr/share/luci/menu.d $(1)/usr/share/rpcd/acl.d
	$(INSTALL_DATA) $(CURDIR)/files/luci/menu.json $(1)/usr/share/luci/menu.d/luci-app-monokulo-push-relay.json
	$(INSTALL_DATA) $(CURDIR)/files/luci/acl.json $(1)/usr/share/rpcd/acl.d/luci-app-monokulo-push-relay.json
endef

# The handle key is made here, so it exists (to back up) from the start.
# LuCI reads its menu and ACLs at login, and rpcd its plugins at start:
# drop LuCI's caches and reload rpcd so Services > Push relay shows up (or
# goes) without a reboot.
#
# On an upgrade, restart the relay so the new binary runs: the SDK's own
# script (default_postinst, which runs before this) only calls start, and
# procd leaves a running instance alone when its command line is unchanged.
define Package/monokulo-push-relay/postinst
#!/bin/sh
[ -n "$${IPKG_INSTROOT}" ] && exit 0
/etc/init.d/monokulo-push-relay secrets
rm -f /tmp/luci-indexcache* /tmp/luci-modulecache/* 2>/dev/null
/etc/init.d/rpcd reload 2>/dev/null
if [ "$${PKG_UPGRADE}" = 1 ] && /etc/init.d/monokulo-push-relay running; then
	/etc/init.d/monokulo-push-relay restart
fi
exit 0
endef

# Removal leaves the data folder, the secrets and the settings' backup
# (the operator's: the install page says how to delete them), and takes
# the firewall rule the package made.
define Package/monokulo-push-relay/postrm
#!/bin/sh
[ -n "$${IPKG_INSTROOT}" ] && exit 0
if [ "$${PKG_UPGRADE}" != 1 ] && uci -q get firewall.monokulo_push_relay >/dev/null; then
	uci -q delete firewall.monokulo_push_relay && uci -q commit firewall
fi
rm -f /tmp/luci-indexcache* /tmp/luci-modulecache/* /tmp/monokulo-push-relay.check /tmp/monokulo-push-relay.blocked 2>/dev/null
/etc/init.d/rpcd reload 2>/dev/null
exit 0
endef

$(eval $(call BuildPackage,monokulo-push-relay))
