FROM node:22-bookworm-slim

ENV PORT=8080
WORKDIR /app
COPY package.json package-lock.json /app/
RUN npm ci --omit=dev --ignore-scripts
COPY app.mjs /app/app.mjs
RUN useradd --create-home --uid 1000 publisher \
	&& chown -R publisher:publisher /app
USER publisher
EXPOSE 8080
CMD ["node", "/app/app.mjs"]
