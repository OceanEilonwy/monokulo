#!/usr/bin/env bash
set -euo pipefail

if ! command -v docker >/dev/null 2>&1; then echo 'missing prerequisite: docker' >&2; exit 2; fi
if ! command -v rg >/dev/null 2>&1; then echo 'missing prerequisite: rg' >&2; exit 2; fi
plugin_dir="$(pwd)/plugins/woocommerce"
if ! test -f "$plugin_dir/vendor/bin/phpunit"; then
  echo 'missing prerequisite: WooCommerce vendor/bin/phpunit (run composer install in plugins/woocommerce)' >&2; exit 2
fi

container="${COVERAGE_WP_CONTAINER:-}"
if test -z "$container"; then
  while read -r candidate; do
    if docker inspect "$candidate" --format '{{range .Mounts}}{{.Source}}{{"\n"}}{{end}}' | rg -Fx -q "$plugin_dir"; then
      container="$candidate"
      break
    fi
  done < <(docker ps --filter 'name=tests-wordpress' --format '{{.Names}}')
fi
if test -z "$container"; then
  echo 'missing prerequisite: running wp-env tests-wordpress container mounting this plugin' >&2; exit 2
fi
base_image="$(docker inspect "$container" --format '{{.Config.Image}}')"
mapfile -t database_environment < <(docker inspect "$container" --format '{{range .Config.Env}}{{println .}}{{end}}' | rg '^WORDPRESS_DB_')
docker_environment=()
for item in "${database_environment[@]}"; do docker_environment+=(-e "$item"); done
docker build -q -t monokulo-coverage-php:local --build-arg "BASE_IMAGE=$base_image" \
  - < "$plugin_dir/coverage.Dockerfile"

run_php() {
  docker run --rm --user "$(id -u):$(id -g)" --volumes-from "$container" --network "container:$container" \
    -v "$COVERAGE_OUTPUT:/coverage" -w /var/www/html/wp-content/plugins/monokulo \
    -e WP_TESTS_DIR=/wordpress-phpunit -e XDEBUG_MODE=coverage "${docker_environment[@]}" \
    monokulo-coverage-php:local "$@"
}

if ! run_php php -r 'exit(extension_loaded("xdebug") && in_array("coverage", xdebug_info("mode"), true) ? 0 : 1);'; then
  echo 'missing prerequisite: Xdebug coverage mode in derived tests-cli image' >&2
  exit 2
fi
run_php php -dauto_prepend_file=/var/www/html/wp-content/plugins/monokulo/coverage-filter.php \
  vendor/bin/phpunit --configuration phpunit.coverage.xml --path-coverage \
  --coverage-html /coverage/html --coverage-xml /coverage/xml \
  --coverage-clover /coverage/clover.xml --coverage-php /coverage/coverage.php \
  --log-junit /coverage/junit.xml
run_php php coverage-summary.php
mv "$COVERAGE_OUTPUT/html/"* "$COVERAGE_OUTPUT/"
rmdir "$COVERAGE_OUTPUT/html"
test -s "$COVERAGE_OUTPUT/index.html"
test -s "$COVERAGE_OUTPUT/xml/index.xml"
