FROM python:3.11-slim

ENV PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1 PORT=8080
WORKDIR /app
COPY app.py /app/app.py
RUN useradd --create-home --uid 1000 publisher
USER publisher
EXPOSE 8080
CMD ["python", "/app/app.py"]
