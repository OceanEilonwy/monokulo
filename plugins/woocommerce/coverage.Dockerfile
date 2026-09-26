ARG BASE_IMAGE
FROM ${BASE_IMAGE}

# wp-env's stock tests-cli image lacks branch-capable coverage. This derived
# image is used only by the coverage command, never by ordinary plugin tests.
RUN pecl install xdebug && docker-php-ext-enable xdebug
