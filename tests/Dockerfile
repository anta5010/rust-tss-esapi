FROM tpm2software/tpm2-tss

ENV PKG_CONFIG_PATH /usr/local/lib/pkgconfig

# Download and install TSS 2.0
RUN git clone https://github.com/tpm2-software/tpm2-tss.git --branch 2.3.1
RUN cd tpm2-tss \
		&& ./bootstrap \
		&& ./configure \
		&& make -j$(nproc) \
		&& make install \
		&& ldconfig

# Download and install TPM2 tools
RUN git clone https://github.com/tpm2-software/tpm2-tools.git --branch 4.1
RUN cd tpm2-tools \
		&& ./bootstrap \
		&& ./configure --enable-unit \
		&& make install

# Install Rust toolchain
RUN curl https://sh.rustup.rs -sSf | bash -s -- -y
ENV PATH="/root/.cargo/bin:${PATH}"
