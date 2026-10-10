FROM maven:3.9.11-eclipse-temurin-21 AS build
WORKDIR /build
COPY pom.xml .
RUN mvn -B -ntp dependency:go-offline
COPY src ./src
RUN mvn -B -ntp package

FROM eclipse-temurin:21-jre
WORKDIR /app
COPY --from=build /build/target/document-parser-1.0.0.jar /app/parser.jar
USER 10001:10001
ENV GEO_PDF_BIND=0.0.0.0 \
    GEO_PDF_PORT=8080 \
    GEO_PDF_MAX_CONCURRENT=1 \
    JAVA_TOOL_OPTIONS="-Xmx320m -Djava.io.tmpdir=/tmp -Duser.home=/tmp"
EXPOSE 8080
ENTRYPOINT ["java", "-jar", "/app/parser.jar"]
