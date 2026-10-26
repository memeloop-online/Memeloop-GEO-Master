FROM mcr.microsoft.com/playwright:v1.63.0-noble

WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci --omit=dev --ignore-scripts --no-audit --no-fund
COPY src ./src
COPY assets ./assets

USER pwuser
ENV GEO_BROWSER_RUNNER_HOST=127.0.0.1
EXPOSE 38080
CMD ["node", "src/server.mjs"]
