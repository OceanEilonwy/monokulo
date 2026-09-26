ARG BASE_IMAGE
FROM ${BASE_IMAGE}

# The source wp-env image may or may not already enable Xdebug. This image is
# used only by the coverage command, never by ordinary plugin tests.
RUN if ! php -m | grep -iq '^xdebug$'; then pecl install xdebug && docker-php-ext-enable xdebug; fi
